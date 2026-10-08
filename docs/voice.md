# Voice

Voice calls run over Matrix (Element X) with LiveKit media. A user links a
Matrix account in Settings → Connections → Matrix, then either calls Note from
that DM or lets Note ring them. A call works like Chat: it answers in the
language spoken (English or Japanese), runs lookups as background jobs while the
talk continues, can be interrupted, and hangs up after a goodbye or 30 minutes.
Note's last words always play out before a call ends.

## Processes

```
note-server  <── unix socket (voice-proto) ──>  note-voice  <── Matrix + LiveKit ──>  phone
                                                    │
                                      HTTP on 127.0.0.1 ──> TTS sidecars
```

- `note-server` owns the conversation, the agent and the call logic
  (`server/src/voice/`). It listens on `[voice] socket`.
- `note-voice` (`voice/`) is the bot's Matrix device: it invites linked accounts
  to a DM, answers and places calls, and runs speech locally: streaming STT
  (sherpa-onnx zipformer), voice-activity and turn detection, spoken-language
  identification, Kokoro TTS, and any configured sidecars.
- `voice-proto/` is the framed protocol and journal between the two.
- Speech sidecars speak a small HTTP protocol
  ([streaming speech design](superpowers/specs/2026-10-03-streaming-speech-design.md)):
  `note-tts-chatterbox` (`tts-sidecar/`, Python, NVIDIA GPU, port 8890; voices
  cloned from CC0 clips listed in [tts-sidecar/voices/README.md](../tts-sidecar/voices/README.md))
  and `note-tts-ja` (`tts-ja/`, Rust, port 8891; see
  [tts-ja/README.md](../tts-ja/README.md) for engines, environment and
  licences).

## Server side

```toml
[voice]
socket = "/run/note/voice.sock"
# model = "…"                # chat model for calls; default: the server's own
# provider_sort = "latency"  # OpenRouter provider.sort for call requests
# reasoning_off = true       # sends reasoning: {enabled: false}; false for a model that refuses it
# first_token_ms = 8000
# max_jobs = 8
# job_timeout_secs = 60
# max_wakes = 4
# wake_settle_ms = 600
```

The call prompt is `prompts/voice.md`. Per-user settings: `ring_for`
(`urgent`, `checkins`, `never`), `voice_voice`, `voice_cue` (the ready sound),
`language`.

Routes (session cookie):

- `POST /api/voice/link` starts or restarts the link: the voice service invites
  the Matrix account to a fresh DM, and the link turns `linked` when the invite
  is accepted. `DELETE /api/voice/link` unlinks.
- `POST /api/voice/test` rings the linked phone now, whatever `ring_for` says.
- `GET /api/voice/voices` lists the call voices, with each sidecar voice's
  `credit`; `GET /api/voice/preview` plays a sample.

## note-voice

Configured by `note-voice.toml` (path in `NOTE_VOICE_CONFIG`, default
`./note-voice.toml`):

| Key | |
|---|---|
| `homeserver`, `token_file` | the bot account; the token alone in the file |
| `livekit_service_url` | the homeserver's LiveKit JWT service |
| `socket` | the server's `[voice] socket` |
| `state_dir` | the bot's Matrix store |
| `models_dir` | speech models; default `NOTE_VOICE_MODELS` |
| `models`, `language_id` | per-language model sets / language identifier replacing those found in `models_dir` |
| `device` | `auto` (default), `cuda` or `cpu` |
| `cues_dir`, `ready_cue_file`, `heard_cue_file` | call sounds; default `NOTE_VOICE_CUES` |
| `tts.sidecars` | `[{id, url}]`; an id prefixes its voice ids, so it is unique and has no `:` |
| `tts.base` | language → sidecar id that is that language's base voice when no Kokoro covers it |

`note-voice-selfcheck` checks the models in `NOTE_VOICE_MODELS` on
`NOTE_VOICE_DEVICE`.

Build and test only inside `nix develop .#voice`, which provides the LiveKit
WebRTC build, sherpa-onnx/onnxruntime and the models.

## On NixOS

`services.note.voice`, `services.note.tts.chatterbox` and
`services.note.tts.japanese` ([deployment.md](deployment.md#voice-and-speech))
generate `note-voice.toml`, wire the socket (`/run/note/voice.sock`), state
(`/var/lib/note-voice`), models, cues and sidecars, and set `[voice] socket` on
the server.
