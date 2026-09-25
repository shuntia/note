# Horizon UI — Product Spec

**Goal:** Replace the Daylight web UI with Horizon: one home screen with two faces (in a session, between sessions), Today living underneath it, and every other view cut to what the user needs right now.

**Visual truth:** `docs/superpowers/mockups/horizon/*.dc.html` (open `index.html` in a browser; each board is a live page). Where this document and a board disagree on a value, the board wins for look, this document wins for behavior.

**The display rule (applies to every element):** does the user need to know this RIGHT NOW, and can they not infer it any other way? If no, it is not on screen. Copy is never explanatory. The interface does not narrate itself.

---

## Global

- One fixed palette. No time-of-day theming. Light: sky gradient `oklch(84% .035 230) → oklch(91% .025 200) 45% → oklch(93.5% .016 78) 72%` on home/session, the brighter `oklch(72% .075 232) → oklch(84% .05 205) → oklch(91% .055 85)` on desktop Today; ground `--earth oklch(93.5% .016 78)`; surfaces `--haze-strong oklch(99% .004 80 / .86)`; ink `oklch(23% .03 255)`; quiet `oklch(40% .025 250)`; faint `oklch(52% .02 245)`; line `oklch(30% .03 250 / .45)`; track `oklch(30% .03 250 / .16)`; sun `oklch(80% .15 76)`; sun-ink `oklch(50% .13 55)`; sage `oklch(50% .09 150)`; rose `oklch(55% .07 30)`. Dark variant: derive by the same roles, keep it warm, no near-black; never red.
- Type: Bricolage Grotesque (display: headings, event names, counters, 500) over Atkinson Hyperlegible (body). Both self-hosted; no external requests. Counters use tabular figures.
- Controls: one filled control per screen (ink pill, 54 px tall on desktop, 48 on mobile). Later/secondary = haze pill. Overflow = three dots. Radii 16 · 18 · pill. No borders on surfaces.
- Mobile: no wordmark, no screen titles; five tabs (Today, Tasks, Chat, Memory, Settings). Desktop: wordmark `Note` (no dot), nav pills, capture bar `Jot anything` + `N`.
- Motion: the session arc breathes (opacity .55↔1, 7 s). Digits drift (counter spec). Everything respects `prefers-reduced-motion`.
- Undo instead of confirm. Dropped things are past tense, rose at most.

## Home (mobile)

Home is ONE screen. Today is what lives under it, reached by swiping up. There is no "Today" landing screen on mobile; the Today tab shows Home.

### In a session
- Face: the 240° arc (open at the bottom, 9 px stroke, track `--track`, progress `--sun`, no tip, no sun), remaining time `mm:ss` (58 px) in the centre, the step name under it (18 px, quiet), `k of n` (12 px, faint). A chevron at the bottom edge. Nothing else. Never fades.
- Swipe up once: the arc shrinks (230 px) to the top; beneath it a round pause button (36–44 px, haze, pause glyph), then `Done with this step` (filled, full width), then the `Tell Note` line. Pause toggles to a play glyph; while paused the counter freezes and the arc dims to 50%.
- Swipe up twice: compact header (120 px arc with the counter, step name, `k of n`), then Today (below).
- Over time: the arc completes and holds, the counter counts up as `+m:ss` in `--sun-ink`. No colour change of the sky. No notification.
- Done on the last step completes the task (undo toast) and returns Home to the between-sessions face.
- End is not a button. Ending is done by telling Note ("stop", "end this") or by finishing.

### Between sessions
- Face: the same arc, whole thing at 38% opacity, filling from the end of the previous session (or the start of the current gap) toward the next event. Centre: `NEXT` eyebrow (12.5 px, tracked, sun-ink), `18 min` (50 px), the event name (18 px, quiet), the span `15:30 – 15:45` (12 px, faint). Beneath: `Start` (filled), `Later` (haze), dots. Chevron at the bottom edge.
- Setting `Arc between sessions` (default on). Off: the face shows the next thing as text (name, `in 18 min`, span) with the same buttons.
- Swipe up: compact header (120 px faded arc with `18 min`, `NEXT` eyebrow, name, span, a small `Start`), then Today.
- Start on a routine begins a session timed to its span; Start on a task uses its duration.
- Later opens a picker: `Later by` 5 · 10 · 15 · 30 · 60 min. Picking snoozes the event by that many minutes (existing snooze semantics with a minute argument).
- Dots: `Drop today`, `Move to tomorrow`, `Silent`. Drop = existing drop with undo toast. Move to tomorrow = drop today + note for the nightly plan to carry it. Silent = turn this day's bell off (`POST /api/events/{id}/alert`); the template keeps its own setting, changed under Settings → Routines and blocks.

## Today (under Home on mobile; a page on desktop)

- The day line: 06:00–24:00, hour ticks, hour labels at 06 · 09 · 12 · 15 · 18 · 21 · 24 (mobile: 06 · 15 · 24). Elapsed part drawn as a 3 px ink line at 70%; ahead as the 1 px line. The now marker is a 14 px ink disc.
- Only what is ahead is drawn: every future event as a span (filled ink pill for the next one, outlined for the rest), labelled `HH:MM – HH:MM Name`. Blocks are spans too. Nothing past is drawn: no done, no snoozed, no dropped, no moved.
- Every event has a span. Routines get an end time (default start + 15 min when the template has none).
- Below the line: the upcoming list (`15:30 – 15:45  Afternoon check-in`, next one bold), then on mobile the `Tell Note` box, then the tabs.
- Desktop Today: hero (`NOW 15:12 · UP NEXT`, name 72 px, `in 18 min` + span, `Start` / `Later` / dots), the day line, the folded morning letter. No "tomorrow's plan" line. No push/flexibility copy.
- The morning letter stays as a single folded row on desktop only.

## Tasks

- `Add a task` field first. Enter saves; toast with Undo.
- `NOW` (≤ 3): rows with circle, title, duration chip (`≈ 25 min` or `Note will estimate` sub-line), a sun Start circle. A split task shows `n steps · k done` and its steps indented with minute counts; the parent's Start opens the next open step.
- `LATER · n`: quiet rows with a dots menu (`Move to Now`, `Drop`).
- `Done today · n`: one folded line at the bottom; tap expands.
- No section for anything else.

## Chat

- Bubbles: Note = haze, user = ink. Under a Note message, each change it made is one receipt line (`sage check · Wind-down walk → 17:30 – 18:15 · chevron`), expandable to the tool call.
- Composer: `Tell Note` with an arrow. The same composer appears on Home (first swipe, in a session) and under Today on mobile; a message sent from there lands in the same thread.

## Memory

- `Search what Note knows`, then plain fact rows with a date. Desktop: master-detail; the detail card shows the fact, `From a chat on <date>`, and `That's wrong` / `Tell Note more`. `That's wrong` posts a chat message to Note asking it to correct or forget the fact (no direct delete).

## Settings

- HOME: `Arc between sessions` (toggle), `Counter` (remaining | elapsed).
- DAY: `Routines and blocks` (count → the existing per-entry list with ping toggles), `Nightly letter` (time), `Time zone`.
- REACH: `Push on this phone` (toggle), `Calls` (toggle, disabled with `Needs a number` until voice exists).
- NOTE: `How Note talks` (persona editor), `Theme` (System | Light | Dark).
- Nothing else. Save is inline (`✓ Saved`), never a dialog.

## Server needs (called out for the plan)

1. Routine template entries gain an end time (or duration) so every event has a span; plan events carry `end_wall_time` for routines as well as blocks.
2. Per-user settings gain `show_arc_between_sessions: bool` (default true) and `counter: remaining | elapsed` if not already present.
3. Snooze accepts a minute count.
4. "Move to tomorrow" needs a way to carry a dropped event to the next plan (an event flag the nightly planner reads, or a task).
5. Session pause: client-side is enough for v1 (paused time is excluded from elapsed); persisted session state gains `paused_at`.
6. Memory "That's wrong" is a chat message; no new endpoint.

## Acceptance (whole suite)

- [ ] Mobile Today tab opens on Home; Home shows the gauge in a session and the wait arc otherwise; swiping up reveals the stages exactly as above and nothing on the face is a button.
- [ ] The session face never fades; over time counts up in sun-ink, nothing turns red.
- [ ] The day line draws only future events, all as spans; every routine has an end time.
- [ ] Later by N snoozes by N; Drop today / Move to tomorrow / Silent behave as specified and Drop is undoable.
- [ ] `Arc between sessions` off shows the text face.
- [ ] No sun disc, no dot next to the wordmark, no explanatory copy anywhere, no time-of-day colour shift.
- [ ] `pnpm build` clean, `cargo test` green, zero external requests.
