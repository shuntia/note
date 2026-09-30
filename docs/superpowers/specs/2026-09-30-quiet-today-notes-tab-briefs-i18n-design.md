# A quieter Today, a Notes tab, briefs on Now, and translation plumbing

Date: 2026-09-30. Builds on the branch that carries the Now face, Inbox, Notes
and Idle nudges plans. Design rules are the ones in
`2026-09-22-ui-audit-and-redesign-design.md` (D0) and
`2026-09-25-sundial-design.md` (E): the face is the one loud element, form over
words, the sun the only accent, and motion only in the orchestrated moments
(the face morphing into the day, the sent bubble's flight).

Eight groups, each shippable on its own, in this order.

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
  task**: the step's title is the face name, the first line of the step's
  notes is the line under it. The parent's title is not shown.
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
appear on the resting Now face when the morning is open.

### When

The brief shows on the resting face (no session, Today on its face, not
scrolled) when either holds:

- **Open morning:** local time is between the day's first activity and 12:00,
  and nothing on today's plan starts within the next 90 minutes (no pending
  block, routine or calendar entry whose start is ≤ 90 min away and none
  running).
- **At a set time:** the local time is within 30 minutes after one of the
  user's `brief_times` (a new user setting, list of `HH:MM`, empty by default).

It stops showing once it has been read today (opened and closed, or dismissed),
remembered per device in localStorage under the letter's date (the review's
`week_start`).

### What

- The morning letter shows when there is one dated today. The weekly review
  shows when its `week_start` is this week's or last week's Monday and it has
  not been read; when both are due, the letter first, the review after it has
  been read.
- **Form:** the resting face keeps its circle, wait arc and next block, so the
  morph is untouched. Under the circle sits one quiet line: a small sun-ink
  mark and the letter's first sentence (the review: its week range), ellipsed
  to one line. Tapping it opens the letter as a sheet over the face (the
  `Overflow` sheet styling: scrim, handle, full text, scrollable). Closing the
  sheet marks it read.
- The desktop `DebriefFold` / `ReviewFold` in the day list stay as the place to
  re-read it.
- The line fades out with the rest of the face in the scroll morph, like the
  hint.

### Server

- `UserConfig.brief_times: Option<Vec<String>>`, each validated by
  `templates::valid_time`, at most 4. Settings GET/PUT carry `brief_times`.
- No new route: the client already has `/api/debrief`, `/api/review` and
  `/api/day` (for the next block).

### Settings

Under **Day**, a row **Briefs** whose value is the times (`07:30, 19:00`) or
`Morning` when the list is empty. The fold holds up to four time inputs and a
`+`.

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
  formatters, the brief-window decision (a pure `briefDue(now, day, times,
  read)`), and the string check.
- Server: `brief_times` default, validation and round trip.
- Harness at 390×844 and 1440×900: the resting morph at scroll 0 / 50 / 100 %,
  the Notes tab, the icon-only bar, a two-step session (one arc, step as the
  name), a toast over the face, and the brief line and sheet at 08:00 with
  nothing for two hours.

## Open questions

1. **Notes tab contents:** notes as the first group above the tasks (this
   spec), or notes only, with tasks moved elsewhere?
2. **Step context:** the parent task's title is dropped from the face. Keep it
   as a faint line instead?
3. **Morning:** "between first activity and 12:00" — or a fixed window, say
   05:00–11:00?
4. **i18n:** is the two-pass split acceptable, and which second language, if
   any, should the first pass prove against (Japanese)?
