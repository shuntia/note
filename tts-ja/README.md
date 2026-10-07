# note-tts-ja

Note's Japanese speech sidecar: a Rust binary speaking the sidecar protocol of
`docs/superpowers/specs/2026-10-03-streaming-speech-design.md` on
`127.0.0.1:8891` (`services.note.tts.japanese`). It serves two voices, both
`"languages": ["ja"]`, at 48 kHz in whole chunks:

| id       | label                  | engine                                   | credit shown in the voice sheet                                                  |
|----------|------------------------|------------------------------------------|-----------------------------------------------------------------------------------|
| `ami`    | 小春音アミ             | Style-Bert-VITS2 JP-Extra, onnxruntime on the GPU | `Style-Bert-VITS2モデル: 小春音アミ、あみたろの声素材工房 (https://amitaro.net/)` |
| `himari` | 冥鳴ひまり（VOICEVOX） | VOICEVOX CORE 0.17, style 14, on the CPU | `VOICEVOX:冥鳴ひまり`                                                              |

VOICEVOX loads first (about a second) and the sidecar reports ready then; each
voice falls back to the other engine when its own is missing or fails on a
chunk, so a stream never goes silent. Style-Bert-VITS2 runs on the GPU, or on
the CPU when there is none and it keeps up (real-time factor ≤ 0.5).

Text is NFKC-normalised, markdown, links and emoji are dropped, and Latin words
become katakana before either engine sees them: a short list in `src/text.rs`
first, spelled-out letters for acronyms, and VOICEVOX's
[kanalizer](https://github.com/VOICEVOX/kanalizer) for the rest.

## Environment

| variable                    | default (from the Nix wrapper)      |                                         |
|-----------------------------|-------------------------------------|-----------------------------------------|
| `NOTE_TTS_HOST`, `NOTE_TTS_PORT` | `127.0.0.1`, `8891`            |                                         |
| `NOTE_TTS_DEVICE`           | `cuda`                              | `cpu` keeps Style-Bert-VITS2 off the GPU |
| `NOTE_TTS_JA_SBV2`          |                                     | `off` leaves both voices to VOICEVOX    |
| `NOTE_TTS_JA_SBV2_DIR`      | `deberta.onnx`, `tokenizer.json`, `model.onnx`, `style_vectors.json` | |
| `NOTE_TTS_JA_VOICEVOX_DIR`  | `lib/`, `dict/`, `model.vvm`        |                                         |
| `ORT_DYLIB_PATH`            | note-voice's CUDA onnxruntime       |                                         |

## Provenance and licences

Everything is fetched at a pinned revision by `nix/tts-ja.nix`; nothing is
downloaded at run time.

- **Style-Bert-VITS2** ([litagin02/Style-Bert-VITS2](https://github.com/litagin02/Style-Bert-VITS2),
  AGPL-3.0 code, unmodified): used only at build time, in `convert/`, to
  export the アミ model to ONNX on CPU torch (adapted from
  [sbv2-api](https://github.com/neodyland/sbv2-api)'s `convert_model.py`, MIT).
  Neither it nor torch is in the runtime closure.
- **sbv2_core** 0.2.0-alpha8 (MIT; its Japanese front end ports
  Style-Bert-VITS2's g2p under LGPL-3.0, as noted in its source): text
  analysis and inference at run time. The optional LGPL AivisSpeech
  dictionary is not built in.
- **jpreprocess** with NAIST-JDIC (BSD-3-Clause): the OpenJTalk front end.
- **小春音アミ** ([litagin/sbv2_koharune_ami](https://huggingface.co/litagin/sbv2_koharune_ami)
  @ `7af50fba`): trained on あみたろの声素材工房's corpora. Use must follow
  [あみたろの声素材工房の規約](https://amitaro.net/voice/voice_rule/): no
  age-restricted, religious/political/MLM or defamatory use, the voice is never
  presented as amitaro's or anyone else's, and published audio carries the
  credit above.
- **DeBERTa** `ku-nlp/deberta-v2-large-japanese-char-wwm`, as ONNX from
  [neody/sbv2-api-assets](https://huggingface.co/neody/sbv2-api-assets) @
  `595cc201`: CC-BY-SA-4.0.
- **VOICEVOX CORE** 0.17.0 C API (MIT), **voicevox_onnxruntime** 1.17.3 and
  the **VVM** voice models 0.16.4 (`1.vvm`, which holds 冥鳴ひまり): under
  the VOICEVOX terms (`TERMS.txt` of each release) and the character's terms
  (linked from [冥鳴ひまり's page](https://voicevox.hiroshiba.jp/product/meimei_himari/)); audio
  made with it is credited `VOICEVOX:冥鳴ひまり`.
- **OpenJTalk dictionary** `open_jtalk_dic_utf_8-1.11` (BSD-3-Clause), for
  VOICEVOX.
- **kanalizer** @ `a65240e5` and its model `VOICEVOX/kanalizer-model` v5: MIT.
- **onnxruntime** with CUDA: note-voice's sherpa-onnx build, shared with it.

The credits reach the web voice sheet through the `credit` field of `/info`,
note-voice's `VoiceOption` and `GET /api/voice/voices`; the sheet shows the
chosen voice's credit under the grid.
