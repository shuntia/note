# UI audit and redesign — 2026-09-22

Every screen of the web client was captured at 1440×900, 1024×700, 800×1000 and
390×844, light and dark, against a copy of a real database (100+ tasks, 40 threads,
223 memories). This document records what the audit found and the design that
answers it. Implementation is split into the tasks at the end.

## Part 1 — Findings

### Cross-screen

| # | Finding | Where |
|---|---|---|
| X1 | The same actions on a plan block are offered three ways: hero buttons + `⋯`, inline `Start Done ⋯` on every list row, and a centred modal dialog from the calendar. | Today, Calendar |
| X2 | Menus differ per screen: `Overflow` popover (Today, Tasks, Chat), `SheetMenu` (Calendar sheet), `BlockMenu` modal (Calendar grid). None adapts to touch: a popover with 0.9rem rows is the phone's menu too. | all |
| X3 | Three-way choices are spread across three menu rows (`Announce: None / Chat / Notify`) in every task menu on Today and Tasks. | Today, Tasks |
| X4 | Chips are two styles: `≈ 25 min` is a bordered pill, `due Oct 31` and `overdue` are bare text. Step durations are bare numbers with no unit (`10`, `15`). | Tasks |
| X5 | The tick (done control) exists on Tasks only; Today marks a block done with a text button. | Today vs Tasks |
| X6 | The desktop top bar vanishes after two idle seconds on Home and for the whole of a session: a screenshot of Home at rest shows no navigation at all. | Home |
| X7 | On the phone the tab bar is hidden at the top of Home (the face) and for the whole face of a session; navigation appears only after a scroll. | Home mobile |
| X8 | Large screens are used as phones: settings, memory and chat stop at 560–680px and leave 55–60 % of a 1440px screen empty; assistant bubbles are capped at 300px wide while the trace under them is 1050px. | Chat, Memory, Settings |
| X9 | Titles of threads are the first message truncated (`note that I need to do the 8 short a…`), check-in threads are titled with the whole check-in question, and every work session leaves an empty `Session: …` thread in the list (three identical rows after three sessions). | Chat |
| X10 | Tool receipts cover 16 of 44 tools; the rest read `Used task_read`; a `batch` reads `Used batch`. | Chat trace |

### Today

| # | Finding |
|---|---|
| T1 | **Bug.** With a session running, or on the phone, the *Close the day* card and the *So far today* fold paint over the big face at the top of the page (they are inside the stage but are not faded in by the morph). |
| T2 | The wait is written as a bare minute count: `932 min` for a check-in fifteen hours away. |
| T3 | Mobile list rows show `18:00 – 18:10 │ 7 G… Start Done ⋯`: the row's three controls leave 30px for the title. |
| T4 | The face name is the whole `task · step` string (four wrapped lines inside the ring); the list row repeats the task title on every step of the same task. |
| T5 | The `Later` picker is a floating panel with five bare numbers and a trailing `min` — reads as a fragment. |
| T6 | `Ping me / Silent` toggles alerting from inside the `⋯` with no indication of the current state. |

### Calendar (on Today's ground)

| # | Finding |
|---|---|
| C1 | **Bug.** A new entry cannot be given a kind: the kind picker lives in the `⋯` menu that only exists for an existing entry. Every new entry is `fixed`. |
| C2 | **Bug.** Task blocks in the week grid get a 30-minute minimum height, so consecutive 10–15-minute blocks stack over each other into an unreadable pile (also on the `06–24` line). |
| C3 | `School` blocks are solid ink slabs (solid ivory in dark mode) that dominate the week; `Crimson Meeting` (busy) is an outline; free time is a dashed outline. Fixed time has no reason to be the loudest thing on the grid. |
| C4 | In the entry sheet the *Quiet* label, the one-off date pill and the switch share a row; the pill reads as the switch's label. |
| C5 | A block's actions open a centred modal dialog; a calendar entry's open a bottom sheet; both from the same grid. |

### Tasks

| # | Finding |
|---|---|
| K1 | Fifty-plus empty grey progress bars: one per row and step, most at 0 %. |
| K2 | Every parent's steps are always expanded; the page is 3400px tall at 1440 and unbrowsable. |
| K3 | With nothing in Now the first heading is `LATER · 34`: the primary list is called "later". |
| K4 | `Keep as one task` is jargon for "remove the steps". |
| K5 | `Start` (the play glyph) exists only on Now rows; a Later row cannot be started without moving it first. |

### Chat

| # | Finding |
|---|---|
| H1 | No header: no thread title, no way to tell which thread is open, the drawer hides behind an unlabelled `⋯` beside the composer (reads as "more", not "chats"), *New chat* is reachable only inside the drawer. |
| H2 | Empty state is a blank screen with a composer at the bottom. |
| H3 | The pulse pill floats over the thread's top-right corner and covers bubble text at 800px. |
| H4 | Thread rows carry the truncated first message, a summary and a date but no title. |

### Memory

| # | Finding |
|---|---|
| M1 | Episodic rows show their raw summary: `2026-09-21 · note I have ap phys c test on fri: The user mentioned they had an AP Physics C test on Friday` — date, thread title and summary all in one string, beside a date column that says the same date. |
| M2 | A fact's body opens with front matter as prose: `tags: […]  valid_from: …  source: …  confidence: high  knowit: …`. |
| M3 | No way to tell episodic from semantic, no filter. Search results (`Turn in all paper`, `Illegible work not graded`) show no source. |

### Settings

| # | Finding |
|---|---|
| S1 | Groups mix concerns: `HOME` holds the session arc, pomodoro, close-the-day and the counter; `NOTE` holds the user's name, the prompts and the theme; `SECURITY` and `API TOKENS` each hold a single fold under a heading of their own. |
| S2 | `Nightly plan` (toggle) and `Nightly letter` (time) are two rows for one feature; `Check-ins` and `Check-ins from Note` sit side by side and mean different things. |
| S3 | Values are inconsistently cased (`Off`, `remaining`, `up to 4 a day`, `1`). `Routines and blocks · 1` shows a count as a value. |
| S4 | `Arc between sessions` is the app's private name for the ring on Home. |
| S5 | `Test notification` is a full row with its own button between two switches. |

### Admin

Left as is. Two notes for later: the gate has no link to the security fold it names, and the inspect group's SQL fold is a text area with no examples.

## Part 2 — Design

### D0. Visual language

The identity stays: the page is a day (sky above the horizon, earth below), the face
is the one loud element, and the sun is the only accent. What changes is discipline.

- **Colour roles.** Sun / sun-ink mean *time and attention* only: the arc, the wait,
  a task block's left rule. Sage means done. Rose means overdue or destructive. Ink is
  text and the single filled button of a screen. Fixed calendar time is ink at 14 %,
  never a solid slab. Nothing else carries colour.
- **Type.** Bricolage Grotesque, weight 500, sentence case, for the counter, the face
  name and every screen or section title. Atkinson for everything read or pressed.
  Scale in rem: 4.5 hero, 1.375 section title, 1 body, 0.875 secondary, 0.78 meta.
  Tabular numerals wherever a number sits in a column.
- **No chrome tells.** No tracked all-caps labels (`NOW`, `LATER · 34`, `HOME`,
  `CLOSE THE DAY`, `SO FAR TODAY` all become sentence-case display headings; the face
  eyebrow becomes the word *Now* or *Next* in sun-ink). No middle-dot meta strings:
  `2 steps · 0 done` → *0 of 2 steps done*; a count follows a heading as a quiet
  numeral (*Later 34*, the numeral in `--faint`). No bordered chips: a duration or a
  due date is quiet tabular text after the title, overdue in rose.
- **Rows.** One row is one line. A row's controls are a tick and a `⋯`; everything
  else is in the menu. A menu group row reads like a settings row: label left, current
  value right in `--quiet`, chevron (*Announce ……… Notify ›*).
- **Words.** Controls say what happens and keep their name through the flow
  (*Move to tomorrow* → toast *Moved to tomorrow*). Empty states invite an action in
  one line; errors say what happened and what to do.
- **Motion.** Only the two orchestrated moments the app already has (the face
  morphing into the day; the sent bubble's flight). Menus, sheets and folds answer a
  press with a short settle and nothing else moves on its own.

### D1. One menu everywhere

`Overflow` becomes the single action menu of the app and takes over the calendar's
`BlockMenu` and `SheetMenu`.

- **Presentation follows the device.** On `(min-width: 768px) and (pointer: fine)` it is
  the popover it is now. Otherwise it is a bottom sheet (`.sheet` styling, scrim,
  handle, 2.75rem rows), which is what a phone expects from a `⋯`.
- **Items carry a `kind`**: `action` (default), `danger` (rose text, always last,
  separated), `radio` (a check mark shows which holds).
- **Groups fold in place.** An item may carry `children: OverflowItem[]`. It renders as
  one row with the label left and the checked child's label right in `--quiet`, then a
  chevron (*Announce ……… Notify ›*); tapping it expands the children under it (radio
  rows) and the chevron rotates. So a three-way choice is one row until it is asked for.
- **A header is optional**: `title` and `subtitle` props render a first non-interactive
  row (the block's name and time, where the calendar's modal had them).
- Keyboard: Escape closes; arrow keys move between rows; the first row takes focus.

### D2. One action vocabulary for a block

Wherever a task block appears (hero, Today list, Calendar grid, the line) it offers the
same set, in this order: **Start**, **Done**, then in the menu **Move to tomorrow**,
**Drop today**, **Announce ›** (None / Chat / Notify, radio), and for routines
**Ping me** (radio-like toggle showing its state with a check).

- **Hero / compact header**: `Start` filled, `Done` haze (routine: `Later` haze), `⋯`.
- **List rows** (Today): `time │ ◯ label … ⋯`. The tick (shared component, moved out of
  Tasks into `web/src/tick.tsx`) is *Done*. `Start` is the first row of the menu and,
  on desktop, a play glyph that appears on hover/focus at the row's right, before `⋯`
  (the glyph Tasks already uses). No inline `Start`/`Done` text buttons in rows.
- **Calendar grid / line**: a block opens the same `Overflow` (with title + time as
  header) anchored to the block, instead of the modal.
- **Tasks rows** keep their tick and `⋯`; the menu becomes: Start, Move to Now / Later,
  Announce ›, Merge steps (was *Keep as one task*), Drop (danger). Start applies to any
  live row.

### D3. Today

- **T1 fix**: the close-day card and the so-far fold are faded in by the compact
  timeline like the list rows are (`from(.close-day, .sofar …)` at 0.42), so nothing
  paints over the face at progress 0. In the desktop hero branch they are inside
  `.today-line` already.
- The face eyebrow is the word *Now* or *Next* (display face, sun-ink), not tracked caps;
  *Close the day* and *So far today* are sentence-case display headings.
- **T2**: `eventFacts` gains `wait: string` for the face: `≤ 90 min` → `N min`;
  otherwise `H h` (rounded to the half hour: `2½ h` → written `2.5 h`). The hero keeps
  `in <wait>`; the ring number shows `<wait>`.
- **T4**: `rowLabel` returns `{ name, of }`: `name` is the step when there is one, else
  the task title; `of` is the task title when the row is a step. The face shows `name`
  as the ring name and `of` as the sub-line above the span; list rows show
  `name` in ink and `· of` after it in `--faint`, truncating `of` first
  (`flex: 0 1 auto; min-width: 0`).
- **T5**: *Later* becomes an `Overflow` group (radio-less action rows `5 min`, `10 min`,
  `15 min`, `30 min`, `1 h`), opened from the `Later` haze button, so it inherits the
  sheet on the phone and the popover on the desktop.
- **X6**: the idle rest keeps fading the `.rest` action groups, but the top bar only
  dims to `opacity: 0.35` and never loses pointer events. During a session on desktop
  the same: dim, not gone.
- **X7**: the phone's tab bar is always visible: the morph no longer tweens `.shell >
  .tabs` from 0, and shell.css no longer hides it before `morph-ready`. The big face
  sits above it; the chevron stays.
- The list's task rows keep their sun-ink left rule; the time column stays.

### D4. Calendar

- **C1**: the kind is a segmented control in the sheet for new and existing entries
  (`Fixed · Busy · Note · Free`), placed under the days row. The `⋯` keeps only *Skip
  this day* and *Delete* (danger) and now uses `Overflow`.
- **C4**: the sheet's rows, top to bottom: title; time; days (a one-off shows its date
  pill *in place of* the day letters, with a small `Repeat` link that swaps the pill for
  the letters and back); kind; quiet (label + switch only, shown for fixed/busy, a
  one-line hint under it for note/free); Save.
- **C2**: the grid's minimum block height is 0.25 h; a block under 0.5 h hides its label
  and shows the title in the tip. On the `06–24` line the same events keep their
  natural width with a 0.4 % floor and no overlap correction.
- **C3**: fixed = `color-mix(in oklch, var(--ink) 14%, transparent)` fill with an ink
  left rule and ink label; busy = the same at 7 % with no rule; note = a dot; free =
  dashed `--sun-ink` outline as now; task blocks unchanged. Skipped = struck label at
  50 % opacity. Dark mode follows from the tokens.
- **C5**: see D1/D2.

### D5. Tasks

- **K1**: a bar is drawn only when it says something: progress > 0, the row is a parent
  with steps, or the row is in Now. Otherwise it appears on hover/focus of the row
  (desktop) so it can still be dragged; on the phone a 0 % bar is not drawn and progress
  is set from the menu (`Progress ›` group with 25 / 50 / 75 %). Step rows follow the
  same rule.
- **K2**: a parent's sub-line (`2 steps · 0 done`) is the fold toggle, with a chevron.
  Now-group parents open by default; Later-group parents start closed. The open set is
  component state (not persisted).
- **K3**: the *Now* section always renders, headed *Now* and *Later 34* in the display
  face, sentence case, the count a quiet numeral. Empty, Now shows one muted line:
  *Nothing in Now yet. Up to three tasks you are on right now.*
- **X4**: duration, due date and overdue are quiet tabular text after the title (`.meta`,
  `--faint`; overdue in rose, no border); step durations read `10 min`. The parent
  sub-line reads *0 of 2 steps done*.
- **K4/K5**: menu as in D2.

### D6. Chat

- **H1**: a thread header row across the column: left a `Chats` button (list glyph +
  label, `aria-expanded`), then the title (generated; `New chat` before the first
  reply; editable on click — the rename input the drawer has today), right a
  `+` *New chat* round button and the pulse. The composer's `⋯` goes away. The pane's
  top mask starts under the header (H3).
- **Desktop ≥ 1088px**: the thread list is a left column (17rem) in the flow rather than
  a drawer, toggled by the `Chats` button; its open state persists in `localStorage`
  (`note.chatListOpen`, default open). Below 1088px it stays the drawer.
- **H2**: the empty thread shows one centred muted line, `Tell Note anything — a task,
  a plan, a question.`
- **Bubbles**: assistant turns take up to 100 % of the column and lose the bubble
  background (plain prose with a 2px `--line` left rule on hover only); user turns keep
  their ink bubble at ≤ 72 %. Traces and receipts share the column width.
- **H4 / X9**: rows show the title (generated, see D9), the gist, the date; `Session:`
  threads and check-in threads with no user reply are not listed (server, D9).
- The list reloads on the shell's `refresh` counter, so a title generated after the
  reply lands without a manual reload.

### D7. Memory

- **M1**: `displaySummary(hit)`: for `episodic`, strip a leading `YYYY-MM-DD · ` and a
  leading `<thread title>: ` (everything up to the first `: ` when the prefix is under
  80 chars). The date column already shows the date.
- **M3**: a segmented filter above the list: *All · Facts · Episodes*
  (`category` param of the list endpoint: `semantic` / `episodic`); hidden while a
  search query is active. Each row carries a small category glyph at its left
  (a dot for a fact, a clock for an episode).
- **M2**: the detail parses leading `key: value` lines (until the first blank line)
  into a metadata row of chips (`tags` split on commas; other keys as `key · value`)
  and renders the rest as the body.
- Desktop: the reading pane is unchanged.

### D8. Settings

Regrouped; every row keeps its current behaviour.

```
SESSIONS     Pomodoro ›            Off / 25 / 5 min
             Counter ›             Remaining
             Show the wait as an arc   [switch]   (was "Arc between sessions")
DAY          Routines and blocks › (value: the template name, not the count)
             Nightly plan ›        On · 03:00   (fold: the switch + the time)
             Close the day ›       21:30
             Time zone ›           UTC
NOTIFICATIONS
             Push on this phone    [switch]
             Telegram ›            @bot / Not linked   (only when enabled)
             Check-ins             [switch]  sub: Routines on the day reach you when they fire
             Check-ins from Note › Up to 4 a day
             (fold body of Check-ins from Note ends with a "Send a test" link + status)
YOU          Your name ›
             Theme ›               System
ADVANCED     How Note talks ›
             Passkeys and codes ›
             API tokens ›
ADMIN        Admin panel ›         (admins only)
             Sign out              (a haze button, not a bare link)
```

Values and group headings are sentence case, headings in the display face. The column
widens to 40rem on desktop.

### D9. Generated thread titles (server)

- Migration v36: `ALTER TABLE conversations ADD COLUMN title_kind TEXT NOT NULL
  DEFAULT 'draft' CHECK (title_kind IN ('draft','generated','user'))`. `PATCH
  /api/conversations/{id}` (rename) sets `user`.
- **First-reply pass.** After `run_turn` persists a turn, when the conversation's
  `title_kind` is `draft` and the server's LLM is a real provider
  (`state.providers_info.llm.is_some()`), a `spawn_blocking` task makes one tool-less
  `chat` call (the shape `search::summarize` uses, `background: true`) with the new
  prompt `title` (`config/defaults/prompts/title.md`, added to `prompts::EDITABLE`):
  system = the prompt; user message = the first user turn and the first assistant reply,
  each clipped to 1500 chars. The reply is normalised: trimmed, surrounding quotes and a
  trailing full stop removed, whitespace collapsed; rejected when empty, over 60 chars,
  more than 8 words, or containing a newline or a `(`. Accepted → `UPDATE conversations
  SET title, title_kind='generated' WHERE id AND title_kind='draft'` (a rename in the
  meantime wins) → `hub.send(user_id, {"kind":"changed"})`. Failures are logged as
  `title_error` and leave the draft.
- **Check-in threads** are titled `<Weekday>'s <HH:MM> check-in` at creation
  (`talk::checkin_thread`) and keep `draft`, so the first user reply re-titles them the
  same way.
- **Work-session threads** are created with `Session: <title>` as now; the pass
  re-titles them after their first exchange.
- **Summary pass.** `summary_write` gains an optional `title` (≤ 60 chars, same
  normalisation); when the idle summary lands, a `title` is stored unless `title_kind`
  is `user`. The `summarize` prompt asks for it in one added sentence.
- **Listing** excludes conversations with no `talk_messages` row (empty session
  threads) — `WHERE EXISTS (SELECT 1 FROM talk_messages m WHERE m.conversation_id =
  c.id)`. The API row gains `title_kind`.

### D10. Tool calling, organised (server + receipts)

- **Registry by domain.** `tools/mod.rs` defines the session registries from named
  domain slices (`MEMORY_READ`, `MEMORY_WRITE`, `TASK_READ`, `TASK_WRITE`, `PLAN`,
  `SCHEDULE`, `CALENDAR_READ`, `CALENDAR_WRITE`, `TRIGGERS`, `CONTEXT`, `SEARCH`,
  `BATCH`, and the one-tool terminal sets) concatenated per kind in a fixed domain
  order, so two kinds that share a domain offer the same tools in the same order. Tool
  names do not change (transcripts and prompts depend on them); the test
  `schemas_cover_the_registry_and_are_objects` and the surface tests still pass.
- **Receipts for every tool.** `web/src/receipts.ts` becomes a table keyed by tool with
  a `{ doing, done, failed }` triple, grouped by the same domains, covering all 44 tools
  (`task_read` → *Read a task*, `task_list` → *Looked over your tasks*, `task_search` →
  *Looked for “…” in your tasks*, `plan_tasks` → *Laid N tasks onto <day>*, `plan_auto`
  → *Filled <day>'s free time*, `calendar_add` → *Added “…” to your calendar*, `say` →
  *Said: …*, `stay_quiet` → *Stayed quiet*, `web_search` → *Searched the web for “…”*,
  and so on). A tool outside the table still gets *Used <name>*.
- **Batch unpacked.** A `batch` step renders one receipt row per inner call
  (`args.calls[i]` with `result.results[i]` when the result carries an array; the
  server's batch result shape is read, not assumed), under a one-line head *Did N things
  at once*. The raw call stays a chevron away as today.

### D11. Chips, ticks, and other shared bits

- `.meta` (styles.css): quiet tabular text, `0.78rem`, `--faint`; `.meta.warn` rose. No
  borders.
- `web/src/tick.tsx` exports `Tick` (moved from Tasks) so Today and Tasks share it.
- `Overflow` as in D1 lives in `overflow.tsx`; its sheet styling reuses `.scrim` and
  `.sheet` from calendar.css, which move into styles.css as shared rules.

## Part 3 — Implementation split

Phase A (parallel, disjoint files):

1. **Server: titles** — db.rs (v36), talk.rs, api.rs (list filter, rename, row field),
   summaries.rs + tools/summary_ops.rs (+ `title`), prompts.rs, new
   `config/defaults/prompts/title.md`, `summarize.md` sentence, tests.
2. **Server: registry by domain** — tools/mod.rs only, tests kept green.
3. **Web shared** — overflow.tsx (D1), tick.tsx, receipts.ts (D10), styles.css (menu
   sheet rules, `.chip`, shared `.scrim/.sheet`), calendar.css (remove the moved rules).

Phase B (parallel): 4. **Today + Calendar** (Home.tsx, Calendar.tsx, events.ts,
receipts.ts's `eventLabel` untouched, calendar.css, home-motion.css, shell.css, the
home section of styles.css). 5. **Tasks** (Tasks.tsx, tasks.css only).

Phase C (parallel): 6. **Chat** (Talk.tsx, talk.css, types.ts, app.tsx; no edits to
styles.css — overrides go in talk.css). 7. **Memory + Settings** (Memory.tsx,
memory.css, Settings.tsx, the settings section of styles.css).

Phase D: screenshots at the four widths again, fix-ups, `cargo test`, `pnpm build`.
