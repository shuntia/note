# Voice calls

Note rings the user's iPhone through Element X and holds a spoken conversation;
the user can also call Note from the same Matrix DM. Speech recognition and
synthesis run locally; the reply model stays on OpenRouter.

## Goals

- A real ring: CallKit's incoming-call screen with a ringtone, on iOS.
- Both directions: Note calls for check-ins and pressing reminders; the user
  calls Note from the DM.
- Fast replies: about 0.6–1.0 s from the end of the user's turn to the first
  audio, by gathering everything before the call and fetching as little as
  possible during it.
- Speech while acting: Note keeps talking while tools run.
- A Note ↔ voice link that loses nothing and applies nothing twice.
- English now, with every language-dependent piece behind a seam.

## Non-goals

- PSTN, Telegram or Discord calls.
- Media end-to-end encryption: the DM is unencrypted, so Element X joins the
  call unencrypted.
- Group calls, video, screen share.
- A local reply model.

## Verified groundwork

A throwaway PoC (2026-09-29) against the live homeserver established:

- `@note:uwu.shuntia.net` (Tuwunel) rang Element X on iOS through CallKit, and
  the user answered. Recipe: put the bot's `org.matrix.msc3401.call.member`
  state (key `_<mxid>_<device>_m.call`, LiveKit focus, `expires`), then send
  `m.rtc.notification` with `notification_type: "ring"`, `sender_ts`,
  `lifetime`, `m.mentions {user_ids: [user], room: true}` and an
  `m.reference` to the membership event. Emptying the membership state (`{}`)
  ends the call.
- Element X rings only when the room has an active call and the ring is under
  15 s old; the mention makes the homeserver push it.
- The bot's OpenID token buys a LiveKit JWT from `lk-jwt-service`
  (`POST https://matrix-rtc-uwu.shuntia.net/sfu/get` with
  `{room, openid_token, device_id}`) with `canPublish` and `canSubscribe`.
- Element X uses per-participant media keys only in encrypted rooms.
- Cloudflare rejects some default HTTP user agents with error 1010; every
  client sends its own.

## Architecture

```
 iPhone (Element X) ──Matrix──▶ Tuwunel ◀──CS API── note-voice ──UDS── note-server ──▶ OpenRouter
        │                                              │
        └──────────── WebRTC audio ──── LiveKit SFU ◀──┘
```

Three units, one repo:

- **`note-voice`** (new binary, own systemd unit): Matrix signalling, LiveKit
  media, the audio pipeline (VAD, streaming STT, turn detection, TTS, playout),
  the call state machine, and an on-disk journal.
- **`note-voice-proto`** (new library crate): every frame both sides exchange,
  the protocol version, and the framing codec. Both binaries depend on it, so
  the wire format cannot drift.
- **`note-server`**: a `voice` module owning call sessions (brief, the reply
  loop, tools, transcripts), the voice channel in the delivery ladder, the
  `Call` session kind with its registry, and a streaming chat method.

The reply loop stays in Note, next to the prompts, tools, traces and database.
The voice process owns everything real-time. A crash in the media stack cannot
take Note down, and only `note-voice` is granted the GPU.

## The Note ↔ voice link

### Transport

- A Unix domain socket, `/run/note/voice.sock`, created by `note-server`,
  mode 0660, group `note`. No TCP port, nothing on the network.
- One long-lived connection, initiated by `note-voice`, carrying
  length-prefixed frames (u32 big-endian length + JSON), 1 MiB maximum.
- `Hello {proto, instance}` in both directions. A version mismatch closes the
  connection with a logged reason, and neither side rings. `instance` is a
  per-process random id, so each side notices when the other restarted.
- `Ping`/`Pong` every second. Three missed pongs mark the link down, and the
  voice side reconnects with jittered backoff capped at 2 s.

### Frames

- **Requests** (`Request {id, body}` → `Response {id, result}`): prepare a
  call, link a Matrix ID, report status. They are idempotent by construction:
  each carries the key of the thing it creates (a call id, a link id), and
  repeating it returns the first answer.
- **Call streams**: every frame belonging to a call is
  `CallFrame {call_id, seq, body}` with `seq` counting up per direction. The
  receiver acknowledges with `Ack {call_id, dir, seq}` (cumulative).
  - The sender writes a frame to durable storage before sending it: the voice
    side to its per-call journal (`/var/lib/note-voice/journal/<call>.ndjson`,
    fsync per frame), Note to the `voice_frames` table.
  - On reconnect each side sends `Resume {call_id, last_acked_seq}` for every
    open call, and the other re-sends everything after it.
  - The receiver drops any `seq` it has already applied. Delivery is
    at-least-once and the effect exactly-once.
  - Acknowledged frames are pruned when the call closes.

### Exactly-once effects

- Every tool call made during a call is keyed by
  `(call_id, turn, call_index)`.
- `voice_ops(call_id, op_key PRIMARY KEY, result_json)` is written in the same
  SQLite transaction as the tool's effect. A replayed turn finds its result
  there and does not run the tool again.
- The conversation rows (user utterance, assistant text, tool rows) go into
  the call's thread as they happen. A turn resumed after a Note restart
  continues the agent loop from those rows instead of starting over.

### Supervision

- Both units: `Restart=always`, systemd watchdog (`WatchdogSec=10`,
  `sd_notify` from the event loop).
- Open calls are rows in `voice_calls`. A restarted Note re-adopts them when
  the voice side resumes, and closes any the voice side no longer knows.
- A restarted voice process re-reads its journals. A call whose media session
  died cannot be resumed: it is closed cleanly (the membership is emptied) and
  the transcript so far is replayed to Note.

### Degraded paths

| Failure | Behaviour |
|---|---|
| Link down when Note wants to ring | The voice channel reports failure; the ladder falls through to push. |
| Link drops mid-call | Voice plays a pre-synthesized line ("I lost my notes for a moment"), keeps listening and journaling. If the link returns within 10 s the turn resumes. Otherwise Note says it will message, hangs up, and replays the journal once the link is back. |
| Reply model errors or times out (8 s to first token) | One retry, with a pre-synthesized "one moment" played meanwhile. After a second failure, an apology line and the turn is dropped. |
| GPU unavailable or out of memory | The same models on the CPU for that call. |

### Fault-injection tests

A test harness wraps the socket and, per case, cuts the connection mid-stream,
duplicates frames, reorders across a reconnect, drops acks, and restarts
either side mid-turn. Each case must leave the same thread rows, the same
tool effects (counted), and the same spoken frames as a clean run.

## Before the call

`PrepareCall {call_id, user, reason, direction, language}`:

1. **Brief.** Note builds the fixed prefix of the prompt once:
   - the voice persona (`voice.md`),
   - the reason (check-in body, trigger text, or "the user called"),
   - today's and overdue tasks with ids,
   - the calendar for the next 24 h,
   - active work sessions,
   - the goals,
   - the top memories for the reason,
   - the tail of the relevant thread.

   The brief does not change during the call. Only the turns grow after it,
   so the provider's prefix cache keeps hitting.
2. **In parallel:**
   - Note sends a 1-token warm-up request with the brief, priming the prefix
     cache and the HTTP connection. The connection is kept alive for the call.
   - The voice side loads and warms the STT, TTS, VAD and turn-detector
     models if they are not resident.
   - The voice side takes a LiveKit JWT, joins the room, and publishes its
     audio track.
   - The opening line comes back from Note: a template for a check-in
     ("Hey — it's about <task>"), generated for an inbound call. The voice
     side synthesizes it into a buffer.
3. **Ring or answer.** Outbound: the voice side puts the membership and sends
   the ring. Inbound: the voice side puts its membership, which answers.
   Playout of the buffered opening starts when the user's audio track
   appears.

Preparation is capped at 4 s. Outbound, a slow step delays the ring, never the
first words. Inbound, the user hears a pre-synthesized "hi" while the brief
finishes.

## The audio pipeline

Everything below runs in `note-voice`, on the GPU through onnxruntime's CUDA
provider when available.

- **In.** LiveKit Rust SDK, `NativeAudioStream` at 48 kHz, resampled to
  16 kHz mono.
- **VAD.** Silero VAD (sherpa-onnx).
- **STT.** Streaming Nemotron-speech-streaming-en-0.6b (sherpa-onnx, int8,
  160 ms chunks). Partial hypotheses arrive continuously, with punctuation.
- **End of turn.** A VAD pause of about 200 ms triggers Smart Turn v3 (8 MB
  ONNX, about 12 ms on a CPU) over the last 8 s of the user's audio.
  - *complete*: commit the turn.
  - *incomplete*: wait for more speech, up to a hard cap (1.2 s of silence).
- **Cues.** Short sounds tell the user the call is listening and that each
  phrase arrived. The heard cue plays ahead of the reply.
  - Two recorded cues, both from Ubuntu's Yaru sound theme (`yaru-theme` in
    nixpkgs, CC-BY-SA 4.0), picked by ear:
    - **ready**: `message-new-instant.oga` (0.42 s), played once when the
      call connects and Note starts listening;
    - **heard**: `message.oga` (0.61 s), played at each committed end of the
      user's turn.
  - The Nix package copies both from `yaru-theme` into `note-voice`'s share
    directory, decoded to 48 kHz mono PCM at build time. No copy lives in
    this repo.
  - `[voice] ready_cue_file` and `[voice] heard_cue_file` override them with
    any WAV or Ogg.
  - A draft that is cancelled plays nothing. A backchannel during playout
    plays nothing.
  - The cue is the first item in the playout queue, so the reply's first
    audio follows it without a gap. Barge-in flushes it like any other audio.
  - Per-user `voice.cue` setting (default on).
- **Preemptive generation.** At the first VAD pause the voice side sends
  `Draft {turn, text}`. Note starts the reply stream but:
  - executes no tool until `Commit {turn, text}` arrives,
  - flags the streamed speech frames as draft; the voice side synthesizes
    them but does not play them.

  If the committed text differs from the draft, or the user resumes speaking,
  the draft is cancelled (`Cancel {turn}`) and restarted from the commit.
- **TTS.** Kokoro-82M (sherpa-onnx). Speech text is cut at clause
  boundaries, the first chunk as early as the first clause, and synthesized
  while the next is still streaming.
- **Voice selection.** Kokoro ships many speakers in one model, so switching
  is only a speaker id; no reload.
  - Per-user `voice.voice` setting, carried in `PrepareCall`.
  - The voice side reports what it can offer through a `ListVoices` request:
    each voice's id, label and language, from the model registry.
  - Settings → Voice shows them, with a preview: Note asks the voice side to
    synthesize a sample line, and the web client plays it.
  - An unknown or removed voice falls back to the language's default. The
    pre-synthesized lines are rendered per voice on first use and cached.
- **Out.** A playout queue into a `NativeAudioSource` (48 kHz), which reports
  the playout position so the transcript records what was actually heard.
- **Barge-in.** Speech from the user during playout pauses it at once.
  - If a transcript of more than a backchannel appears within 600 ms, the
    queue is flushed, and the unplayed text is marked unspoken in the thread.
  - "uh-huh / right / okay" alone, or no words at all (a cough or noise),
    resume playout where it paused.
- **Echo.** The phone cancels its own echo, and the SFU never returns the
  bot's own track.

The reply model is tuned for voice: reasoning off, OpenRouter
`provider.sort = "latency"`, a model chosen per `[voice]` config. Every turn
logs its timings into the session trace: VAD end, commit, first token, first
audio, playout start. This makes latency measurable, not assumed.

## Speech while acting

- **`SessionKind::Call`** has its own registry:
  - the task, session, calendar, goal and memory-write tools,
  - `batch` and `web_search`,
  - `respond` and `hang_up`,
  - a small set of lookups as a fallback, since the brief should make them
    rare.
- **Text is speech.** Assistant text streams straight to the voice side as
  `Speak` frames.
- **`respond {text?}`** is voice-only; `text` defaults to "On it...".
  - It resolves immediately: Note emits a `Speak` frame and returns
    `"spoken"`.
  - The persona tells the model to put `respond` in the same batch as any
    tool that takes time.
- **Parallel batches.** Within a batch, calls run concurrently:
  - `respond` and network-bound tools (web search, sub-sessions) overlap
    freely,
  - calls that write to the database serialize on the writer.

  Each result is streamed back as it lands. The next model round starts as
  soon as the last result is in, not when playout finishes.
- **Silence guard.** If a round's tools pass 1.5 s with nothing queued to say,
  the voice side plays a pre-synthesized "one moment".
- **Talking over tools.** If the user speaks while tools run, the committed
  utterance joins the next round's input instead of waiting for, or aborting,
  the running calls.
- **Ending.** `hang_up {}` ends the call after playout drains. So does the
  user leaving, or 30 minutes.

## Calls in both directions

### Linking

Settings → Voice:

1. The user enters their Matrix ID.
2. Note asks the voice side to open the DM: an unencrypted private room,
   `is_direct`, invite.
3. Once the user joins, the link is stored in `voice_links(user_id, mxid,
   room_id, linked_at)`.

The bot joins only rooms it created for a link, and accepts calls only from
the linked Matrix ID in that room.

### Outbound

- A new `voice` channel in the delivery ladder. Per-user setting "Ring me
  for": `pressing` (default) / `check-ins` / `never`, plus the existing quiet
  windows.
- `deliver` returns `Ok` once the voice side accepts the prepare.
- The outcome comes back on the call stream as `Outcome {answered | declined |
  missed | failed}`. Anything but `answered` re-delivers the message through
  the rest of the ladder.
- The ring lasts 30 s.

### Inbound

- The voice side's `/sync` loop watches linked rooms for the user's
  `call.member` state appearing, or for an `m.rtc.notification` addressed to
  the bot.
- It sends `PrepareCall {direction: inbound}`, answers by putting its own
  membership, and greets.
- If the link to Note is down, it answers with a pre-synthesized "I can't
  reach your notes right now" and hangs up. The user is never left ringing
  into nothing.

The `/sync` loop persists its `since` token and filters to linked rooms.

## Language seams

English ships. Everything language-dependent already takes a language:

- a per-user `voice.language` setting (default `en`), carried in
  `PrepareCall`,
- voices listed per language, so the voice picker filters by it,
- `SpeechToText`, `TextToSpeech` and `TurnDetector` traits in `note-voice`,
  resolved from a model registry keyed by language (`[voice.models.en]` in its
  config),
- prompts resolved as `voice.<lang>.md` → `voice.md`, and the opening
  templates as `voice.<lang>` → default,
- pre-synthesized lines keyed by language,
- the backchannel word list keyed by language.

Adding a language means a model entry, a prompt file and a word list; no code.

## Data

New tables, in one migration:

| Table | Holds |
|---|---|
| `voice_links` | Linked Matrix ID and DM room per user. |
| `voice_calls` | Call id, user, direction, reason, event id, conversation id, state, timestamps, outcome. |
| `voice_frames` | Note's outbound call frames until acknowledged. |
| `voice_ops` | Idempotency records for tool calls. |

Transcripts are ordinary conversation rows with `Via::Voice`.

## Configuration and deployment

`server.toml`:

```toml
[voice]
socket = "/run/note/voice.sock"
model = "deepseek/deepseek-v4-flash"   # reply model for calls, reasoning off; defaults to providers.llm
```

`note-voice.toml` (generated by the module):

- the homeserver URL, the bot token file, the JWT service URL,
- the socket path, the models directory, `language_default = "en"`,
- the device (`cuda` | `cpu`).

Nix:

- `packages.note-voice`, built with crane. The prebuilt libwebrtc is fetched
  with `fetchurl` and handed over through `LK_CUSTOM_WEBRTC`. sherpa-onnx is
  taken from nixpkgs with CUDA, or the crate's static build; the plan picks
  one after a build spike.
- `packages.note-voice-models`: pinned `fetchurl`s for Nemotron, Kokoro,
  Silero and Smart Turn.
- `services.note.voice.enable` in the same module:
  - user `note-voice` in group `note`, state `/var/lib/note-voice`,
  - the bot token through LoadCredential,
  - `DeviceAllow` for `/dev/nvidia*`.
- The host adds `/persist/secrets/note/matrix-bot.token`, the token from the
  PoC registration, moved there.

## Testing

- **Protocol.** Codec round trips, version refusal, and the fault-injection
  suite above.
- **Note side.**
  - `Call` registry contents.
  - `respond` default and immediacy.
  - Parallel batch ordering.
  - Draft → commit → cancel: no tool runs on a draft.
  - Idempotent replay through `voice_ops`.
  - Brief contents from a fixture day.
  - The ladder falling through on `missed`.
- **Voice side.**
  - The pipeline driven by recorded WAV fixtures: transcript, turn commits,
    barge-in versus backchannel.
  - TTS chunking.
  - The ready cue plays once per call. The heard cue plays once per committed
    turn and never for a cancelled draft.
  - Voice selection falls back to the default voice.
  - The Matrix client against a mock homeserver: ring, answer detection,
    hang-up, sync resume.
- **End to end.** A manual phone test for each direction, reading the
  per-turn timings from the trace.

## Risks

- **The LiveKit Rust SDK build under Nix.** The prebuilt-webrtc route is
  documented in its build script but untested here. The first plan task is a
  build spike.
- **Nemotron ONNX with the CUDA provider in sherpa-onnx** may need the crate's
  own onnxruntime rather than nixpkgs'. The CPU int8 path is the known-good
  fallback.
- **Element X behaviour can change between releases.** The ring recipe is
  pinned by an integration check that the PoC script becomes: ring, then
  confirm `hasRoomCall` from the bot's own sync.
- **The reply model's time to first token** dominates latency and is outside
  Note's control. The timings in the trace make a model switch a measured
  decision.
