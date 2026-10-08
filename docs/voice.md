# Voice

Voice calls run in the web app, or over Matrix (Element X) with LiveKit media.
In the app, the composer's send button is a mic while the field is empty and
opens a full-screen call ([web.md](web.md#the-pwa)). For Matrix, a user links an
account in Settings → Connections → Matrix, then either calls Note from that DM
or lets Note ring them. A call works like Chat: it answers in the
language spoken (English or Japanese), runs lookups as background jobs while the
talk continues, can be interrupted, and hangs up after a goodbye or 30 minutes.
Note's last words always play out before a call ends.

## Processes

```
browser  <── /api/call/ws ──>  note-server  <── unix socket (voice-proto) ──>  note-voice  <── Matrix + LiveKit ──>  phone
                                                                                   │
                                                                     HTTP on 127.0.0.1 ──> TTS sidecars
```

- `note-server` owns the conversation, the agent and the call logic
  (`server/src/voice/`). It listens on `[voice] socket`.
- `note-voice` (`voice/`) is the bot's Matrix device: it invites linked accounts
  to a DM, answers and places calls, and runs speech locally: streaming STT
  (sherpa-onnx zipformer), voice-activity and turn detection, spoken-language
  identification, Kokoro TTS, and any configured sidecars.
- `voice-proto/` is the framed protocol and journal between the two, version 4;
  each side refuses a peer on another version. A call's `start` carries its
  `origin` (`matrix`, the default, or `web`); a web call's audio and live state
  travel as `media` frames (`audio_in` at 16 kHz, `audio_out` at its `rate`,
  `flush` on barge-in, `state`), PCM as base64 of little-endian 16-bit samples.
  `note-voice` runs a web call's session on that relayed media instead of
  LiveKit.
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
- `GET /api/call/ws` opens a web call ([below](#web-calls)).
- `GET /api/voice/voices` lists the call voices, with each sidecar voice's
  `credit`; `GET /api/voice/preview` plays a sample.

## Web calls

`GET /api/call/ws?conversation_id=&ring=` upgrades to a call socket. Without
`ring` it starts a call into `conversation_id` (when it is the caller's) or a
new thread; with a ring token it answers that ring. Admission is that of
`/api/ws` ([delivery.md](delivery.md#websocket)) with its own cap: two call
sockets per user (`429`); a new call replaces the user's open one.

Browser to server: binary frames of 16 kHz mono s16le, 2 to 3200 bytes (100 ms;
larger is refused at the socket), and `{"type":"mute","on":bool}` or
`{"type":"hangup"}`. Server to browser: `{"type":"open","rate":48000}` first,
then binary 48 kHz s16le audio, `{"type":"flush"}`, `{"type":"state","state"}`
(`listening`, `hearing`, `thinking`, `speaking`), `{"type":"caption","text"}`
for the caller's words, and last `{"type":"ended","reason","conversation_id"}`
with `ended`, `failed`, `replaced`, `busy` (a Matrix call is open),
`unavailable` (no voice service, or its link down for 10 s) or `missed` (the
ring was gone). Five seconds with no audio from the browser hangs the call up;
after a hang-up Note's last words still play out before `ended`. Audio backed
up past 50 frames is dropped; control frames always go through.

Calls are recorded in `voice_calls` with `origin` and `thread_id` (schema v54).
`web/scripts/call-check.mjs` checks the whole path headless
([development.md](development.md#web-development)).

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
