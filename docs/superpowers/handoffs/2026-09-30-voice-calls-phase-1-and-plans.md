# Handoff: voice calls phase 1, and four feature plans

Written 2026-09-30 09:00 PDT. Nothing here needs the session that wrote it:
every step below is complete as written.

## 1. Voice calls phase 1 (link and ring): built, reviewed, not merged

- Branch `voice-calls`, worktree `.claude/worktrees/voice-calls`, 18 commits
  on top of `main` 04e46d9, head 7c5c463.
- Every task passed its own review, and the whole branch passed a final
  review with one fix round. The final re-review found every finding
  addressed.

### What it does

- **Settings → Calls:** link a Matrix ID, accept Note's DM invite in
  Element X, then choose "Urgent" or "Never" and tap **Ring me** to test.
- **Urgent messages ring the iPhone:** check-ins and other high-urgency
  messages ring through CallKit.
  - Answering, declining or missing the call ends it.
  - The message always follows through Telegram or push, so the ring is
    only there to get your attention.
- **Note ↔ voice link:**
  - Note and the new `note-voice` service talk over `/run/note/voice.sock`.
  - Every call message is saved to disk before it is sent and resent until
    acknowledged, and repeats are dropped.
  - Either process can restart mid-ring without losing or doubling
    anything. The tests cover cuts, duplicates, lost acks, restarts and
    silent links.
- **Tests:** everything passes: `nix flake check` (including the NixOS VM
  test with voice on), `cargo test` for the three crates, and the web build
  and tests.

### To deploy (all copy-paste)

1. **Merge.** From `/home/shuntia/Projects/note`:

   ```
   git merge --no-ff voice-calls
   ```

   `main` has four docs-only commits since the branch point, so the merge is
   clean.

2. **Install the bot token.** The bot's credentials were moved out of `/tmp`
   to `~/.local/state/note/matrix-bot.json` (mode 600).

   ```
   python3 -c "import json;print(json.load(open('/home/shuntia/.local/state/note/matrix-bot.json'))['access_token'])" | sudo install -m600 -o root -g root /dev/stdin /persist/secrets/note/matrix-bot.token
   ```

   Then delete the copy: `rm ~/.local/state/note/matrix-bot.json`. The file
   also holds the bot's password, so you may prefer to keep it somewhere
   root-only instead.

3. **Host module.** In `~/Documents/configuration-nix/modules/note.nix`,
   inside `services.note`:

   ```nix
       voice = {
         enable = true;
         tokenFile = "${secrets}/matrix-bot.token";
         settings = {
           homeserver = "https://uwu.shuntia.net";
           livekit_service_url = "https://matrix-rtc-uwu.shuntia.net";
         };
       };
   ```

   In the same file, add to `environment.persistence."/persist".directories`:

   ```nix
       { directory = "/var/lib/note-voice"; user = "note-voice"; group = "note"; mode = "0700"; }
   ```

   Without that entry, the voice service's journal and link state are wiped
   on every reboot.

4. **Build and switch.**

   ```
   cd ~/Documents/configuration-nix && nix flake update note && sudo nixos-rebuild switch
   ```

   The database migrates v41 → v42 at start.

5. **Check the service.** `journalctl -u note-voice -b` should show
   `voice: signed in as @note:uwu.shuntia.net` and no repeated
   `voice link dropped`.

   Also check for sandbox denials: `Permission denied`,
   `Read-only file system`, `Operation not permitted` or a `SIGSYS` exit.

6. **Link and ring.**
   1. In Settings → Calls, link `@shuntia:uwu.shuntia.net`.
   2. Accept "Note" in Element X. This is a new room; the old test DM can be
      left.
   3. Tap **Ring me**. Expected: the CallKit call rings, answering ends it,
      and "This was a test call from Note." arrives through Telegram or push.
   4. Tap **Ring me** again and decline. Expected: the ringing stops at once
      and the message still arrives.

### Known and accepted

The full list, with reasons, is in the ledger:
`.claude/worktrees/voice-calls/.superpowers/sdd/2026-09-29-voice-calls-phase-1-link-and-ring/progress.md`.

- **Ring me while a ring is already running** gives a generic failure (500)
  instead of saying a ring is under way.
- **Answering ends the call at once.** Phase 1 has no audio; talking to Note
  is phase 2 (audio) and phase 3 (conversation), which aren't planned yet.
- **A call is never rung while another call is starting or ringing.** That
  message goes straight to Telegram or push.

### Rulings made on your behalf

Each one says what it costs if it was wrong.

- **Implementers ran one at a time,** because every task built on the one
  before. Cost if wrong: time only.
- **Implementers and reviewers ran on Opus.** The final review also used
  Opus 5.5, per your instruction. Cost if wrong: none.
- **Deployment was not run.** It needs your `sudo` and touches the host.
  Cost if wrong: none.
- **Tasks 1 and 2 were reviewed together.** Cost if wrong: one larger review.
- **A local read error on the voice service counts as "nothing applied yet".**
  At worst a second ring within 10 s after a disk error.
- **Messages for a call Note has deleted are acknowledged and dropped.** This
  is intended.
- **Nothing is delivered until the fallback channels are set up at startup.**
  Messages wait for one reconnect.
- **Migration v42 was edited in place twice.** Production is on v41, so
  nothing is affected there. Any local database already at v42 from this
  branch must be recreated.
- **A database read error on Note counts as a gap.** Cost: one resend round.

## 2. Four feature plans: written, not started

- **Spec:** `docs/superpowers/specs/2026-09-30-postits-inbox-focus-connections-design.md`,
  approved in the session.
- **Plans:** in `docs/superpowers/plans/`, committed at `5edb2af`.

| Plan | Tasks | What it builds |
|---|---|---|
| `2026-09-30-now-face.md` | 5 | Stop appears only while paused; instant start takes today's scheduled tasks before the queue; when idle only the timer shows and everything else is gone, not faded |
| `2026-09-30-inbox.md` | 7 | Each school item is kept with Note's decision and reason; an Inbox section in Memory; a Refresh button that works through a host `.path` unit (the last task is a host change) |
| `2026-09-30-notes.md` | 6 | "Notes": a plain list on Today's resting face (check, add, menu for edit/pin/done), a 7-day undo, and chat tools; styling is left for later |
| `2026-09-30-idle-nudges.md` | 9 | Presence tracking; idle means no session and no activity for 20 min; Note may nudge about open notes, wait, and nudge again; activity or a reply cancels it |

- **Build order:** Now, then Inbox, then Notes, then Idle nudges. Notes
  expects the Now plan's changes to `Home.tsx`, and Idle nudges uses the Notes
  table.
- **Not yet written:** plans 5 (Matrix messaging plus a Connections section in
  Settings) and 6 (translation plumbing). Plan 5 needs voice calls merged
  first, because it renames `voice_links` to `matrix_links`. Plan 6 goes last,
  because it touches every screen.

Each plan's opening sections list the rulings its writer made where the spec
was silent. A few worth checking before you run them:

- **Now:** the Stop control is a round button under the paused circle. Idle
  keeps the arc, the counter and the pause mark.
- **Notes:** the resting face becomes a small start ring with the list below
  it. The chat tools take `note_id`. Deleting is not offered in the web client.
- **Inbox and Idle nudges:** see the "Rulings" section at the top of each plan.

To run a plan, ask for it by name, for example "run the Now plan
subagent-driven".
