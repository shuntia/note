# A quieter Today, a Notes tab, briefs on Now, and translation plumbing

Date: 2026-09-30. Builds on the branch that carries the Now face, Inbox, Notes
and Idle nudges plans. Design rules are the ones in
`2026-09-22-ui-audit-and-redesign-design.md` (D0) and
`2026-09-25-sundial-design.md` (E): the face is the one loud element, form over
words, the sun the only accent, and motion only in the orchestrated moments
(the face morphing into the day, the sent bubble's flight).

Nine groups, each shippable on its own, in this order.

## 1. Today's resting face as it was

- The resting face is the 320 / 440 circle again: the faded wait arc, the
  wait, the next block's name, its `of` line and its span, and the start hint.
  With the arc off, the `home-text` block.
- This is what the scroll morph flies into the compact header (phone) and the
  hero (desktop). The Notes list had replaced it, leaving the resting morph
  with nothing to move.
- Notes leave Today entirely. `Home.tsx` no longer imports `Notes`; the
  `.home-face.at-rest` rules go.
- The Now-face changes stay: candidates, Stop while paused, and idle keeping
  only the timer and its words.

## 2. The Notes tab

- The tab called **Tasks** becomes **Notes** (id `tasks` unchanged, so routes,
  deep links and stored state keep working).
- The screen opens with a **Notes** group, a sibling of *Now* and *Later*:
  the open notes as rows (tick, text, pin glyph when pinned, `Overflow` on
  right-click or long-press: Edit, Pin / Unpin, Done), and the `+` input at its
  foot.
- Rows use the task row's measures (2.5 rem, 0.875 rem gutter) so the two lists
  read as one screen. No colours, tilts or swipes.
- The task add field stays where it is; notes keep their own `+` input.
- The nav glyph stays the checklist.

## 3. The phone's tab bar

- Icons only. Each button keeps its name as `aria-label`.
- Order on the phone: Notes, Chat, **Today**, Memory, Settings, so Today sits
  under the thumb in the middle. The desktop top bar keeps its order and its
  words.
- The glide mark behind the current icon is unchanged. The bar keeps its
  height, so every offset that reads 84 px (the chevron, the toast, the
  compact header) holds.

## 4. Steps are tasks on the face

- A session on a task with steps shows **the current step as if it were a
  task**: the step's title is the face name. Under it, in `--faint`, the
  parent task's title. Idle takes the parent line away with the rest, so a
  resting session shows the timer and the step's name.
- The arc is one continuous span for the step, never split into segments. It
  fills by the step's own clock (`stepFracAt`), as now.
- Swipe down finishes the step and advances to the next as now; the face
  simply shows the next step as a new task.
- The strip's other slots name the step they would start (`entry.step`), else
  the task.
- The compact header and the phone's pulled arc read the same name.
- `Gauge`'s `steps` prop and `segments` stay in `gauge.tsx` / `circle.ts`
  unused by Home; removing them is left to a clean-up.

## 5. Toasts

Glossy and out of the way:

- **Surface:** a translucent pill, `color-mix(in oklch, var(--ivory) 62%,
  transparent)` with `backdrop-filter: blur(18px) saturate(1.6)`, a 1 px
  inner highlight at the top (`inset 0 1px 0 color-mix(white 55%)`), a hairline
  `--line` border and a soft, low shadow. Ink text. Dark mode and Sky follow
  from the tokens.
- **Size:** one line, 0.875 rem text, `max-width: min(22rem, 100vw − 32px)`,
  truncated with an ellipsis rather than wrapped.
- **Place:** top centre on the desktop, under the top bar; on the phone above
  the tab bar as now. Never over the face's centre.
- **Motion:** fades and rises 8 px in 200 ms; leaves the same way. Reduced
  motion: opacity only.
- **Action:** the `Undo` is a quiet text button in `--sun-ink`, no border.
- **Non-blocking:** the pill takes pointer events; the space around it never
  does. It never steals focus.

## 6. Briefs on Now

The morning letter (`/api/debrief`) and the weekly review (`/api/review`)
appear on the resting Now face while the morning is open.

### When

The brief shows on the resting face (no session, Today on its face) when all
hold:

- local time is before the user's `morning_until` (a new setting, `HH:MM`,
  default `11:00`);
- nothing on today's plan is running or starts within the next 90 minutes;
- the brief has not been read today on this device (localStorage, keyed by the
  letter's date or the review's `week_start`).

The morning letter shows when there is one dated today. The weekly review
shows when its `week_start` is this week's or last week's Monday and it is
unread; when both are due, the letter first, then the review.

### Form

- **The message takes the circle's place.** The circle, its wait and the start
  hint leave (fade and settle down 12 px, 0.35 s), and the letter rises into
  the face (0.45 s, `power2.out`): a reading column (`min(34rem, 100vw − 48px)`,
  max height the face's, scrolling inside), a small sun-ink mark, the text.
- A round check button under it marks it read: the letter fades and lowers,
  and the circle comes back with the arc drawing in (`fx.open`'s track fade).
- Scrolling down from the message: the message fades out with the face as the
  hint and chevron do; the compact header and the hero fade in rather than
  morph, since the circle's parts are not on screen. Back at the top the
  message is there again until it is read.
- Reduced motion: opacity only, 200 ms.
- The desktop `DebriefFold` / `ReviewFold` in the day list stay as the place to
  re-read it.

### Server

- `UserConfig.morning_until: Option<String>`, validated by
  `templates::valid_time`; accessor defaulting to `11:00`. Settings GET/PUT
  carry it.

### Settings

Under **Day**, a row **Morning ends** with the time as its value; the fold
holds one time input.

## 7. Translation plumbing

As in section 6 of `2026-09-30-postits-inbox-focus-connections-design.md`,
landing in two passes:

- **Pass 1 (this change):**
  - `web/src/i18n/index.ts` with `t(key, vars?)`, `{name}` placeholders and
    `{count, one {…} other {…}}` plurals through `Intl.PluralRules`.
  - `web/src/i18n/en.ts`, a flat object keyed by view; `Dict` type derived from
    it so another language file is checked for missing keys at compile time.
  - `web/src/i18n/format.ts`: the only date, time, relative-time and number
    formatters, taking the active locale (`en` for now).
  - Every string in the files this change touches moves behind `t()`: the app
    shell and tab bar, toasts, Home, Notes, Tasks, Memory and Inbox, the
    debrief / review / brief, `Overflow`, `jot`.
  - The vitest string check from the spec, enforced on those files; the other
    files are named in an explicit allowlist that the second pass empties.
- **Pass 2 (separate change):** Settings, Calendar, Talk, receipts, Share and
  Admin, then the allowlist is removed. Server notification text moves into
  `server/src/text.rs`.

## 8. Checks

- Web: `pnpm -C web build && pnpm -C web test`; vitest for `t()`, plurals, the
  formatters, the brief-window decision (a pure `briefDue(now, day, until,
  read)`), and the string check.
- Server: `morning_until` default, validation and round trip.
- Harness at 390×844 and 1440×900: the resting morph at scroll 0 / 50 / 100 %,
  the Notes tab, the icon-only bar, a two-step session (one arc, step as the
  name, parent faint, gone when idle), a toast over the face, and the brief in
  the circle's place at 08:00 with nothing for two hours, read and back.

## 9. Clippy pedantic

- The workspace `Cargo.toml` sets `clippy::pedantic` to warn for all three
  crates (`[workspace.lints]`, each crate `lints.workspace = true`), and the
  checks run `cargo clippy --workspace --all-targets -- -D warnings`.
- Allowed at the workspace, with the reason in `Cargo.toml`:
  - `missing_errors_doc`, `missing_panics_doc`: they demand a doc section on
    every fallible function, against the comment policy in `CLAUDE.md`;
  - `must_use_candidate`: an attribute on most getters, with no caller that
    drops the value today;
  - `too_many_lines`: the long functions are route handlers and the agent
    loop, which read top to bottom; splitting them is its own change.
- Every other pedantic finding, and the four default findings already on the
  branch, is fixed in the code.

## Decisions

- Notes sit above the tasks on the Notes tab.
- The parent task stays as a faint line under the step, until idle.
- Mornings end at 11:00 by default, per user.
- No set brief times; briefs are not tied to quiet windows.
- i18n lands in two passes, English only.
