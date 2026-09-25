# Daylight UI Overhaul — Product Spec

**Goal:** Rework Note's web UI around ADHD-first principles: one obvious thing per screen, zero-friction capture, time made visible, forgiveness everywhere, no guilt walls.

**Status:** Approved design. Nothing implemented. Nothing committed (mockups and specs are untracked by design — committing is the user's call).

**Visual truth:** the HTML mockups in `docs/superpowers/mockups/daylight/` (self-contained; open in a browser — several animate). The published proposal artifact ("Note Daylight") carries the same content with commentary. Where this document and a mockup disagree on a value, the mockup wins for look, this document wins for behavior.

**How to execute:** This is the product spec, not the task plan. The design was produced WITHOUT reading the app source (deliberately). The implementing session must first read the relevant code, then draft a file-level implementation plan per `superpowers:writing-plans` arguing from this spec, one plan per step below. Steps are ordered by user-impact per unit of work and each ships alone. Steps 1–4 are frontend-only; steps 5–7 need server/model changes (called out inline).

**Companion spec:** `docs/superpowers/plans/2026-08-31-now-counter-spec.md` — the Now-screen counter, fully specified to the keyframe. Step 6 requires it.

---

## Global constraints (apply to every step)

- Design tokens: keep the existing `--sun` `#e8871e` accent and oklch monochrome scale in `web/src/styles.css`. New in step 10, until then use existing tokens only. Accent = amber only; moss = done; clay = dropped/warn. Never introduce red.
- Copy rules: sentence case; active voice; an action keeps its name through its whole flow (button "Done" → state "done"); user vocabulary, never system vocabulary (no "semantic/episodic/procedural", no "IANA", no tool names in user-facing text).
- Dropped/missed items: recorded quietly, past tense, clay at most. Never alarm styling, never modal interruptions about the past.
- Every state-changing action gets feedback within 100 ms (optimistic UI) and, where destructive or bulk, an in-place Undo. Nothing is confirm-dialog-guarded if it can be undo-guarded instead.
- Accessibility floor: visible keyboard focus on all interactive elements; `prefers-reduced-motion` disables all decorative animation; hit targets ≥ 40×40 px on touch layouts; text contrast ≥ 4.5:1.
- All assets bundled; the client makes no external requests (fonts included — see step 10).

---

## Step 1 — Event action hierarchy (Today)

Mockups: `today-desktop-light.html`, `today-mobile-dark.html`.

Today's plan events currently render five equal text links (Done · Later · +15 · −15 · Drop) on every event at all times. Replace with:

1. Only the CURRENT event (the fired one, or next pending if none fired) shows actions. Past and future events show none — they act via step 2's card when they become current. (Snoozed events re-fire and become current again.)
2. Action hierarchy on the current event, exactly:
   - **Done** — the only filled button (sun background, dark text, pill). Primary.
   - **Later** — outlined pill, muted text, beside Done. (Snooze, existing semantics.)
   - **−15 / +15** — small outline chips, pushed to the far end of the row, only on events whose flexibility allows sliding.
   - **Drop** — NOT in the row. Behind an overflow control (⋯) on the card, together with nothing else for now. Dropping shows a toast: `Dropped "<name>" — moved off today` with an **Undo** button (5 s).
3. Done/Later/Drop keep their existing API semantics; this step is presentation only.

Acceptance:
- [ ] A pending future event renders zero action controls.
- [ ] The current event renders exactly one filled button, and it is Done.
- [ ] Drop requires two interactions (open ⋯, then Drop) and can be undone from the toast.
- [ ] Keyboard: the ⋯ menu is focusable and operable; toast Undo is focusable while shown.

## Step 2 — The Now line and Now card (Today)

Mockups: `today-desktop-light.html` (the dashed rule + sun dot + big card), `today-mobile-dark.html`.

1. The existing current-time rule becomes the **Now line**: a dashed horizontal rule across the spine at the current minute, a 12 px sun-colored disc on the spine axis, and the label `NOW · HH:MM` (small caps style, sun-ink color) sitting on the rule. Updates each minute.
2. The current event (defined as in step 1) renders as the **Now card**: enlarged card directly under the Now line — serif event title (~30 px desktop / 25 px mobile), an eyebrow line above it reading `UP NEXT · IN <n> MIN` (pending) or `NOW` (fired), a meta line (`Can slide ±30 min · reaches you as a push` — derived from flexibility + channel), then the step-1 action row.
3. Past events (done/dropped) render compact and faded above the line: done = moss check + strikethrough name; dropped = plain name + clay-outlined tag `dropped — <where it went>` when the agent rescheduled it, else `dropped`.
4. Future events render compact and muted below the line with their flexibility tag (`±30 min`, `fixed`).
5. The debrief moves from its side panel into a single folded row at the top of Today: `☀ This morning: <first sentence…>` with a chevron; expanding reveals the full letter inline. Collapsed state persists per day in localStorage key `note.debriefFolded`.
6. Below the last event: the quiet line `Tomorrow's plan arrives overnight — nothing for you to set up.`

Acceptance:
- [ ] The Now line is labeled and moves as time passes (verify across a minute boundary).
- [ ] Exactly one event renders as the Now card at any time; when the last event of the day is done, the card region shows `That's everything today.` with no button.
- [ ] Debrief expands/collapses and remembers its state on reload.

## Step 3 — Global capture

Mockup: header bar in every desktop mockup (`Jot anything — a task, a thought, a change of plan` + `N` key chip).

1. A capture bar sits in the app header on every view (mobile: a pill at the top of Today, and reachable everywhere via the same shortcut behavior when a hardware keyboard exists).
2. Pressing `n` anywhere (except when focus is in an input/textarea/contenteditable) focuses it. `Esc` blurs it and restores focus.
3. Submitting (Enter) with non-empty text creates a Task with that text as title (existing quick-add API), clears the bar, and shows toast `Saved to Tasks — Undo` (Undo deletes the just-created task). The user stays on the current view.
4. Placeholder copy exactly: `Jot anything — a task, a thought, a change of plan`. No category picker, no options, no second field. Ever.
5. Draft preservation: unsent text survives view switches and reload (localStorage key `note.captureDraft`, cleared on submit).

Acceptance:
- [ ] `n` focuses capture from all five views; typing `n` inside Talk's composer does NOT.
- [ ] Submit → task exists, toast shows, Undo removes it.
- [ ] Type text, switch views, reload: text is still in the bar.

## Step 4 — Task groups and undo (Tasks)

Mockup: `tasks-desktop-light.html`.

1. Replace the flat list with three groups, in order:
   - **NOW** — header `NOW · <k> of 3`, right-aligned hint `a short list you can actually finish`. Hard cap 3 tasks. Card-styled rows (border + subtle shadow). Which tasks are "now" is a stored per-task flag; the agent may set it; the user moves tasks between Now/Later via the row's ⋯ menu (`Move to Later` / `Move to Now`). Attempting to move a 4th into Now shows toast `Now is full — finish or move something first` and does not move it.
   - **LATER** — header `LATER · <n>`. Visually quieter rows (transparent background).
   - **DONE TODAY** — header `DONE TODAY · <n>`. Tasks completed today only; struck through, moss-filled check circles; borderless rows. Older done tasks are not shown (they remain queryable by the agent).
2. Completion: tapping the circle marks done instantly (optimistic) and shows toast `<title> — done` with **Undo** (5 s). Undo restores previous state AND previous group.
3. The in-view add row keeps title-only add: placeholder `Add a task — just a title is enough`, right hint `press Enter to save`. New tasks land in Later (or Now if Now has room and the list was empty).
4. In-progress state renders as a small sun-tinted tag `in progress` on the row (set via ⋯ menu or by the agent).

Acceptance:
- [ ] Now never renders more than 3 tasks, including after agent writes (excess falls to Later, newest-demoted-first).
- [ ] Done + Undo round-trips a task to its exact prior group and state.
- [ ] Empty states: empty Now shows `Nothing queued — pull something up from Later, or just add what's on your mind.`; a fully empty view shows only the add row and that line.

## Step 5 — Task durations and steps (model + Tasks UI) — needs server work

Mockup: `tasks-desktop-light.html` (the landlord email parent).

1. Model: Task gains `duration_min: Option<u32>` (rough, 5-minute granularity), `duration_source: user | agent | none`, and `parent_id: Option<TaskId>`. Hierarchy is EXACTLY one level deep — a task with a parent cannot be a parent. The API rejects deeper nesting (422).
2. The agent's task tool may: set/adjust durations (5-min granularity), and split a task into 2–5 child steps each with a duration. A split is a suggestion the user can flatten: parent row ⋯ menu → `Keep as one task` deletes children and their durations, keeping the parent.
3. Rendering: duration chip on each row — `≈ 20 min` (solid outline) when set; dashed-outline chip `Note will estimate` when `duration_source: none` and estimation is pending. Parent rows show a sub-line: `≈ <total> min · Note split this into <n> steps · <k> done` and an indented child list (rail-connected) with per-child circles, names, right-aligned `<d> min` chips. Completing all children completes the parent (with the same toast/undo as step 4, undo reopens parent and the last child).
4. A **Start** button (▶, 34 px circle, sun-outlined) on each NOW-group task opens the Now screen (step 6) with that task and its duration (parent → its next open child).

Acceptance:
- [ ] API rejects a grandchild task with 422.
- [ ] `Keep as one` removes children in one action, undoable.
- [ ] Durations only ever display in 5-minute increments.
- [ ] Completing the last child marks the parent done in the same optimistic frame.

## Step 6 — The Now screen (focus mode)

Mockups: `now-screen-dark.html` (LIVE — open it; it animates, ticks, breathes, and click toggles the ambient state), `tick-candidates.html` (the chosen digit transition is D). Counter behavior: implement `2026-08-31-now-counter-spec.md` verbatim.

1. Entry points: the Start button on a task (step 5), tapping the Now card's title area (starts the current event as an untimed session), and it is the resting face when the PWA reopens during an active session.
2. Layout (top to bottom): whisper header (serif `Note` left, `Weekday · HH:MM` right, both faint, NO sun disc in this screen's wordmark); centered 380 px gauge; a quiet outlined `Done` pill; a tiny `Next · <event>, <HH:MM>` line; the pull-up sheet lip at the bottom edge.
3. The gauge: 240° arc, open at the bottom (gap centered), clockwise from the lower-left end, radius such that stroke width is 11 px at the 380 px size, `stroke-linecap: round`. Track = mist token. Progress = linear gradient `#c96a08 → #f6b053` toward the leading tip, with a soft sun-colored drop-shadow glow. Entry animation, two beats: track draws in (0.6 s, from its start, `cubic-bezier(.3,.7,.3,1)`, 0.1 s delay), then progress extends to its position (1.2 s, same family curve, 0.5 s delay). Both via stroke-dash offsets; reduced motion renders both complete instantly. Progress fraction = elapsed/duration clamped to 1; untimed sessions show track only.
4. Center stack: the counter (per the counter spec; 82 px), beneath it the duration underline — a 150 px row reading `<duration>m` between two dashed hairlines; then task title (~15.5 px, quiet), then step line `step <k> of <n> · <child name>` (faint) when the task is a split child.
5. The number breathes: after 30 s without pointer/key activity the center stack blur-fades out (0.8 s, to `blur(12px)` + opacity 0); any activity brings it back (0.8 s in). Swipe down (touch) hides it immediately and pins it hidden; swipe up or tap unpins. `Esc`/`ArrowUp` keyboard-equivalents open the sheet / unhide. Done and Next stay visible in all states. This view sets `overscroll-behavior: none` so swipe-down cannot trigger pull-to-refresh.
6. The pull-up sheet: 250 px wide lip, centered bottom, grabber bar + hint text `Break · End session`. Pulling up (or `Esc`) reveals: **Take a break** (pauses the timer; the arc dims to 50% and the underline row reads `paused`; the sheet button becomes **Back to it**), **End session** (returns to Today; the task keeps its elapsed time noted via the agent-visible task notes; no shame copy — just return), and **Switch the number** (flips elapsed/remaining, same as the setting). Escape-shaped actions live ONLY here; nothing on the main surface defers.
7. Overrun: when elapsed passes duration, the arc completes and holds; the counter keeps counting up in `--sun-ink` color; nothing else changes. No red, no modal, no sound.
8. Completing (`Done`) plays the completion moment: the arc's stroke crossfades amber→moss over 0.5 s, then navigates back to Today. Reduced motion: instant.

Acceptance:
- [ ] Counter spec checklist (all 9 items) passes.
- [ ] Entry beats: track visibly completes before the progress arc finishes extending.
- [ ] 30 s idle hides the numbers; swipe-down pins hidden; Done remains clickable in ambient state.
- [ ] Pause dims the arc and freezes the counter; resume continues from the paused value.
- [ ] Overrun turns nothing red and fires no notification.

## Step 7 — Schedule blocks and per-routine alerts — needs server work

Mockups: `today-desktop-light.html` (Work time band + bells), `settings-desktop-light.html` (Schedule pane).

1. Template model: an entry is either a **routine** (existing point events: fixed / slide / drop flexibility) or a **block** (`kind: block`, start + end time, always agent-reshapable). Routines gain `alert: bool` (default true).
2. Blocks never fire deliveries. Routines with `alert: false` still appear on Today and still track done/snoozed state, but produce no push/WS nudge.
3. Today rendering: blocks render as a dashed-border band on the spine spanning their range, label + time range + tag `flexible — Note may reshape it`; past blocks fade. Routines with alerts show a small bell glyph after their name; `alert: false` shows a struck bell. (Mobile omits the bells; blocks keep a short `flexible` tag.)
4. Settings → Schedule pane, exactly as mocked: Time zone (`Your days start and end here. Type a city to search.`), Nightly debrief (`When Note writes the morning letter and plans tomorrow.`), Shape of the day (`Which routines and blocks make up a day.`), then **Routines & blocks — choose which ones ping you**: one row per template entry — time/flexibility, name, and a toggle pill (`🔔 pings you` sun-tinted / `🔕 silent` muted); block rows show `blocks never ping` instead of a toggle. Footnote: `Silent routines still appear on Today — they just don't send a push.` Save button = filled sun pill (fix the washed-out current style); on success show `✓ Saved` inline (moss), not a dialog.
5. The agent's plan tool may move/reshape blocks freely and must not change `alert` flags.

Acceptance:
- [ ] A silent routine fires no push/WS delivery but appears on Today and can be marked done.
- [ ] Blocks are excluded from delivery scheduling entirely.
- [ ] Toggling a bell persists into the per-user template override and survives the nightly rebuild.

## Step 8 — Tool-call receipts (Talk)

Mockup: `chat-desktop-dark.html`.

1. Each agent tool call renders as a one-line **receipt** chip under the reply: moss check + past-tense human sentence, e.g. `✓ Moved "Wind-down walk" to 17:00`, `✓ Remembered: low-energy Sundays are common lately`. A chevron expands it into a card showing the raw tool name + args/result (existing data) in mono. Failed calls: clay `✕` + `Couldn't <verb>…` + expandable error.
2. Receipt sentences come from a per-tool template map in the client (tool name + args → sentence); unknown tools fall back to `✓ Used <tool name>` — still a chip, never a raw block.
3. Under the composer, the standing line: `Every change Note makes shows up above — nothing happens silently.`
4. Conversation rename/delete move behind a ⋯ menu on the conversation row; delete shows toast `Deleted "<title>" — Undo` (10 s; actual deletion deferred until the toast expires).
5. Assistant messages get the small sun-disc avatar and a measured column (as mocked); no other chat restyling in this step.

Acceptance:
- [ ] No raw tool block is ever the default rendering; every call is a chip first.
- [ ] Delete is two interactions + undoable; nothing is destroyed while the toast shows.
- [ ] Receipt templates cover all current agent tools (enumerate them from the code when planning).

## Step 9 — Vocabulary and Memory surfaces

Mockup: `memory-desktop-light.html`.

Exact string replacements (user-facing only; API/category identifiers unchanged):

| Where | Old | New |
|---|---|---|
| Memory filter | `Semantic` | `About you` |
| Memory filter | `Episodic` | `Moments` |
| Memory filter | `Procedural` | `How you work` |
| Settings timezone help | `Type to search, or enter any IANA zone name.` | `Your days start and end here. Type a city to search.` |
| Settings nightly help | `When the assistant writes the day's letter and plans tomorrow.` | `When Note writes the morning letter and plans tomorrow.` |
| Settings template label/help | `Template` / `The shape of a day — which check-ins get scheduled.` | `Shape of the day` / `Which routines and blocks make up a day.` |
| Chat placeholder | `Message Note...` | `Talk to Note — it can change the plan, tasks, and memory for you` |

Memory view additions (read-only stays read-only):
1. Lede under the title: `What Note has learned as you talk. Nothing here is ever deleted — replaced notes move to the archive.`
2. List rows: summary, category chip (tinted per category: About you = sun-ink, Moments = moss, How you work = clay), relative saved date.
3. Detail pane: category chip, serif summary title, body, rule, then meta lines `Saved from Talk · <relative time>` and, when superseding, `Replaces a note from <date> · see what changed` (link opens the archived fact).
4. `Ask Note about this` button: opens Talk with a new conversation pre-filled (not sent): `About the memory "<summary>" — `.
5. Keep the existing empty-state sentence unchanged (it is already right).

Acceptance:
- [ ] Zero occurrences of "semantic/episodic/procedural/IANA" in rendered UI text (grep the built bundle's strings).
- [ ] Supersede link renders only when a `supersedes` chain exists and resolves to the archived fact.
- [ ] `Ask Note about this` lands in Talk with the draft populated and focus in the composer.

## Step 10 — Tokens, type, and theme pass

Mockups: all (light = paper `oklch(97.5% 0.006 85)` family; dark = soft warm `oklch(17.5% 0.01 60)` family — exact token values are in any mockup's `:root`/dark block; copy them).

1. Adopt the mockups' token values for both themes; the key correction is dark mode: current near-black drifts darker than the design system intends — dark `--bg` becomes the mockups' `oklch(17.5% 0.01 60)` with raised surfaces `21.5%`/`25%`.
2. Bundle two font families as self-hosted `@font-face` (OFL licenses, subsetted, woff2, no external requests): **Fraunces** (weights ~420 & 560, opsz axis) and **Atkinson Hyperlegible** (400/700 + italic 400). Fraunces is display-only: wordmark, view titles, Now card title, Now-screen counter, Memory detail titles. Atkinson replaces the system stack for all UI/body text. Keep the existing mono stack.
3. Spine gradient on Today: the timeline rule becomes a top-to-bottom gradient (morning gold → noon neutral → dusk violet → night ink; exact stops in `today-desktop-light.html` / `today-mobile-dark.html` for the two themes). Decorative only — no information may exist ONLY in this gradient.
4. Sidebar: quieter inactive items (quiet text, no icons color), active item = sunk background + bold + sun-ink icon, as mocked. Mobile tabs gain icons above labels (as mocked).
5. Remove the orange diamond `◆` markers before section headings app-wide; headings stand on typography alone.

Acceptance:
- [ ] Built client makes zero network requests to font hosts (verify in devtools offline).
- [ ] Both themes render every view with tokens only (no literals that exist in one theme).
- [ ] Fraunces appears in exactly the five display roles listed and nowhere else.

---

## Self-review notes (already applied)

- Steps 1+2 both touch the Today event row; they are separated because step 1 is shippable without the card/line and reviewers can gate them independently, but an implementer may fold them into one plan.
- The Now-screen idle state (no active session: big clock + countdown-to-next arc) is deliberately DEFERRED — it is mentioned in the proposal but not specced; do not improvise it. Ship steps 6's session mode only.
- Voice/Twilio, iOS specifics, and Reminders sync remain out of scope, as before.
