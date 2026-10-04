import argparse
import asyncio
import json
import logging
import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path

os.environ.setdefault("TQDM_DISABLE", "1")

import numpy as np
from aiohttp import WSMsgType, web

log = logging.getLogger("note-tts-chatterbox")

SAMPLE_RATE = 24000
FRAME_SAMPLES = SAMPLE_RATE // 4
PEAK = 0.95
DEFAULT_VOICE = "default"
VOICE_SUFFIXES = (".wav", ".flac")


@dataclass
class Settings:
    host: str
    port: int
    model_dir: Path | None
    voices_dir: Path | None
    device: str
    exaggeration: float
    cfg_weight: float
    temperature: float


class Cancelled(Exception):
    pass


def discover_voices(voices_dir: Path | None) -> dict[str, dict]:
    """Voice id -> {"label", "path"}; the built-in voice has no path."""
    voices = {DEFAULT_VOICE: {"label": "Chatterbox", "path": None}}
    if voices_dir is None or not voices_dir.is_dir():
        return voices
    labels = {}
    if (meta := voices_dir / "voices.json").is_file():
        labels = json.loads(meta.read_text())
    for clip in sorted(voices_dir.iterdir()):
        if clip.suffix in VOICE_SUFFIXES and clip.stem != DEFAULT_VOICE:
            voices[clip.stem] = {"label": labels.get(clip.stem, clip.stem), "path": clip}
    return voices


class Engine:
    """One Chatterbox model with per-voice conditionals; renders one chunk at a time."""

    def __init__(self, settings: Settings):
        self.settings = settings
        self.voices = discover_voices(settings.voices_dir)
        self.model = None
        self.conds = {}
        self.ready = False
        self.lock = asyncio.Lock()
        self.cancel_event: threading.Event | None = None

    def load(self):
        import torch
        from chatterbox.tts import Conditionals

        s = self.settings
        t0 = time.perf_counter()
        model_dir = s.model_dir or self._download()
        self.model = _load_model(model_dir, s.device)
        self.model.t3.tfmr.register_forward_pre_hook(self._check_cancel)
        builtin = Conditionals.load(model_dir / "conds.pt").to(s.device)
        builtin.t3.emotion_adv = s.exaggeration * torch.ones(1, 1, 1, device=s.device)
        self.conds[DEFAULT_VOICE] = builtin
        for vid, v in self.voices.items():
            if v["path"] is not None:
                self.model.prepare_conditionals(str(v["path"]), exaggeration=s.exaggeration)
                self.conds[vid] = self.model.conds
        for vid in self.conds:
            self._render("Hello, this is a warm-up.", vid)
        self.ready = True
        log.info("ready in %.1f s with voices %s", time.perf_counter() - t0, ", ".join(self.voices))

    @staticmethod
    def _download() -> Path:
        from huggingface_hub import hf_hub_download

        files = ["ve.safetensors", "t3_cfg.safetensors", "s3gen.safetensors", "tokenizer.json", "conds.pt"]
        return Path([hf_hub_download("ResembleAI/chatterbox", f) for f in files][-1]).parent

    def _check_cancel(self, *_):
        if self.cancel_event is not None and self.cancel_event.is_set():
            raise Cancelled

    def _render(self, text: str, voice: str) -> np.ndarray:
        s = self.settings
        self.model.conds = self.conds[voice]
        wav = self.model.generate(
            text,
            exaggeration=s.exaggeration,
            cfg_weight=s.cfg_weight,
            temperature=s.temperature,
        )
        audio = wav.squeeze(0).numpy().astype(np.float32)
        peak = float(np.abs(audio).max()) if audio.size else 0.0
        if peak > PEAK:
            audio *= PEAK / peak
        return (np.clip(audio, -1.0, 1.0) * 32767).astype("<i2")

    async def render(self, text: str, voice: str, cancel: threading.Event) -> np.ndarray | None:
        """PCM for `text`, or None when `cancel` was set before or during the render."""
        async with self.lock:
            if cancel.is_set():
                return None
            self.cancel_event = cancel
            try:
                pcm = await asyncio.to_thread(self._render, text, voice)
            except Cancelled:
                return None
            finally:
                self.cancel_event = None
        return None if cancel.is_set() else pcm


def _load_model(ckpt_dir: Path, device: str):
    """ChatterboxTTS.from_local, but built and loaded straight onto `device` to keep host RAM low."""
    import torch
    from chatterbox.models.s3gen import S3Gen
    from chatterbox.models.t3 import T3
    from chatterbox.models.tokenizers import EnTokenizer
    from chatterbox.models.voice_encoder import VoiceEncoder
    from chatterbox.tts import ChatterboxTTS
    from safetensors.torch import load_file

    with torch.device(device):
        ve = VoiceEncoder()
        t3 = T3()
        s3gen = S3Gen()
    ve.load_state_dict(load_file(ckpt_dir / "ve.safetensors", device=device))
    t3_state = load_file(ckpt_dir / "t3_cfg.safetensors", device=device)
    if "model" in t3_state:
        t3_state = t3_state["model"][0]
    t3.load_state_dict(t3_state)
    del t3_state
    s3gen.load_state_dict(load_file(ckpt_dir / "s3gen.safetensors", device=device), strict=False)
    for m in (ve, t3, s3gen):
        m.to(device).eval()

    tokenizer = EnTokenizer(str(ckpt_dir / "tokenizer.json"))
    return ChatterboxTTS(t3, s3gen, ve, tokenizer, device)


async def info(request: web.Request) -> web.Response:
    engine: Engine = request.app["engine"]
    return web.json_response({
        "id": "chatterbox",
        "label": "Best",
        "input": "chunks",
        "sample_rate": SAMPLE_RATE,
        "ready": engine.ready,
        "voices": [{"id": vid, "label": v["label"]} for vid, v in engine.voices.items()],
    })


async def stream(request: web.Request) -> web.WebSocketResponse:
    engine: Engine = request.app["engine"]
    ws = web.WebSocketResponse(heartbeat=30)
    await ws.prepare(request)

    voice = DEFAULT_VOICE
    queue: asyncio.Queue[str | None] = asyncio.Queue()
    cancel = threading.Event()

    async def fail(message: str):
        await ws.send_json({"type": "error", "message": message})
        await ws.close()

    async def speak():
        try:
            while (text := await queue.get()) is not None:
                if text.strip():
                    pcm = await engine.render(text, voice, cancel)
                    if pcm is None:
                        return
                    for i in range(0, len(pcm), FRAME_SAMPLES):
                        await ws.send_bytes(pcm[i:i + FRAME_SAMPLES].tobytes())
                if cancel.is_set():
                    return
                await ws.send_json({"type": "mark", "chars": len(text)})
            await ws.send_json({"type": "done"})
            await ws.close()
        except Exception as e:
            log.exception("render failed")
            if not ws.closed:
                await fail(str(e))

    speaker: asyncio.Task | None = None
    try:
        async for msg in ws:
            if msg.type != WSMsgType.TEXT:
                continue
            try:
                m = json.loads(msg.data)
                kind = m["type"]
            except (ValueError, KeyError, TypeError):
                await fail("malformed message")
                break
            if kind == "open" and speaker is None:
                if not engine.ready:
                    await fail("model is still loading")
                    break
                voice = m.get("voice") or DEFAULT_VOICE
                if voice not in engine.conds:
                    await fail(f"unknown voice {voice!r}")
                    break
                speaker = asyncio.create_task(speak())
            elif speaker is None:
                await fail("stream not opened")
                break
            elif kind == "text":
                queue.put_nowait(str(m.get("text", "")))
            elif kind == "end":
                queue.put_nowait(None)
            elif kind == "cancel":
                break
    finally:
        cancel.set()
        queue.put_nowait(None)
        if speaker is not None:
            await speaker
        await ws.close()
    return ws


def env(name: str, default: str) -> str:
    return os.environ.get(f"NOTE_TTS_{name}", default)


def settings_from_args() -> Settings:
    p = argparse.ArgumentParser(description="Chatterbox TTS sidecar for Note")
    p.add_argument("--host", default=env("HOST", "127.0.0.1"))
    p.add_argument("--port", type=int, default=int(env("PORT", "8890")))
    p.add_argument("--model-dir", type=Path, default=os.environ.get("NOTE_TTS_MODEL_DIR"))
    p.add_argument("--voices-dir", type=Path, default=os.environ.get("NOTE_TTS_VOICES_DIR"))
    p.add_argument("--device", default=env("DEVICE", "cuda"))
    a = p.parse_args()
    return Settings(
        host=a.host,
        port=a.port,
        model_dir=a.model_dir,
        voices_dir=a.voices_dir,
        device=a.device,
        exaggeration=float(env("EXAGGERATION", "0.5")),
        cfg_weight=float(env("CFG_WEIGHT", "0.5")),
        temperature=float(env("TEMPERATURE", "0.8")),
    )


def main():
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    logging.getLogger("chatterbox").setLevel(logging.WARNING)
    settings = settings_from_args()

    async def start_loading(app: web.Application):
        engine: Engine = app["engine"]

        def on_loaded(t: asyncio.Future):
            if t.exception() is not None:
                log.critical("model failed to load", exc_info=t.exception())
                os._exit(1)

        app["loader"] = asyncio.ensure_future(asyncio.to_thread(engine.load))
        app["loader"].add_done_callback(on_loaded)

    app = web.Application()
    app["engine"] = Engine(settings)
    app.router.add_get("/info", info)
    app.router.add_get("/stream", stream)
    app.on_startup.append(start_loading)
    web.run_app(app, host=settings.host, port=settings.port, print=None, access_log=None)


if __name__ == "__main__":
    main()
