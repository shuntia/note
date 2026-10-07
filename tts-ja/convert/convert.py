"""Exports a Style-Bert-VITS2 JP-Extra model to the ONNX graph and style-vector JSON that sbv2_core loads.

Adapted from neodyland/sbv2-api scripts/convert/convert_model.py (MIT).
"""

import argparse
import json
import subprocess
from pathlib import Path

import numpy as np
import torch
from style_bert_vits2.models.hyper_parameters import HyperParameters
from style_bert_vits2.models.infer import get_net_g

INPUTS = [
    "x_tst", "x_tst_lengths", "sid", "tones", "language", "bert", "style_vec",
    "length_scale", "sdp_ratio", "noise_scale", "noise_scale_w",
]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("model_dir", type=Path)
    p.add_argument("out_dir", type=Path)
    a = p.parse_args()
    torch.manual_seed(0)
    a.out_dir.mkdir(parents=True, exist_ok=True)

    styles = np.load(a.model_dir / "style_vectors.npy")
    (a.out_dir / "style_vectors.json").write_text(json.dumps({"shape": styles.shape, "data": styles.tolist()}))

    hps = HyperParameters.load_from_json(a.model_dir / "config.json")
    weights = next(a.model_dir.glob("*.safetensors"))
    model = get_net_g(str(weights), hps.version, "cpu", hps)

    def forward(x, x_len, sid, tone, lang, bert, style, length_scale, sdp_ratio, noise_scale, noise_scale_w):
        return model.infer(x, x_len, sid, tone, lang, bert, style, sdp_ratio=sdp_ratio,
                           length_scale=length_scale, noise_scale=noise_scale, noise_scale_w=noise_scale_w)

    model.forward = forward
    n = 21
    args = (
        torch.randint(1, 100, (1, n)),
        torch.LongTensor([n]),
        torch.LongTensor([0]),
        torch.zeros(1, n, dtype=torch.long),
        torch.ones(1, n, dtype=torch.long),
        torch.randn(1, 1024, n),
        torch.from_numpy(styles[0]).unsqueeze(0),
        torch.tensor(1.0),
        torch.tensor(0.0),
        torch.tensor(0.6777),
        torch.tensor(0.8),
    )
    onnx = a.out_dir / "model.onnx"
    with torch.no_grad():
        torch.onnx.export(
            model, args, str(onnx),
            input_names=INPUTS,
            output_names=["output"],
            dynamic_axes={
                "x_tst": {0: "batch_size", 1: "x_tst_max_length"},
                "x_tst_lengths": {0: "batch_size"},
                "sid": {0: "batch_size"},
                "tones": {0: "batch_size", 1: "x_tst_max_length"},
                "language": {0: "batch_size", 1: "x_tst_max_length"},
                "bert": {0: "batch_size", 2: "x_tst_max_length"},
                "style_vec": {0: "batch_size"},
            },
        )
    subprocess.run(["onnxsim", str(onnx), str(onnx)], check=True)


if __name__ == "__main__":
    main()
