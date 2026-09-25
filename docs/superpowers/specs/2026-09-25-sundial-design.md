# Sundial: motion at scale, phone zoom, the Sky theme, device time zone, Begin working

Date: 2026-09-25. Mockups: `docs/superpowers/mockups/sundial/` (serve the repo root and open
`/docs/superpowers/mockups/sundial/index.html`; `sky.js` there is the reference implementation
for the Sky theme's colour maths and `motion.html` shows the list motion before and after).

Five independent workstreams. A and B touch only the web client's motion and styles; C is the
theme; D adds one setting on the server and a hook on the client; E adds one server endpoint and
a new face state on Home.

## A. List motion that stays quick at any size

### What is wrong today
- `settle()` in `web/src/motion-gsap.ts` staggers every row by 0.03 s in DOM order, so a list of N
  rows takes 0.32 + 0.03·N seconds to land, and in the two-column Later list (`tasks.css`, min-width
  1088px) the left column finishes before the right column begins.
- `useRowMotion` in `web/src/views/Tasks.tsx` re-measures every row on every animation frame while
  any row is folding away.
- Every websocket `changed` frame bumps `refresh` in `web/src/app.tsx` at once; a burst fires as many
  full reloads, none cancelled, and a slow older response can overwrite a newer one.

### Design
1. **Entrance as a wave.** `settle(targets, y, within?)` keeps only rows whose rect intersects the
   viewport (or `within`'s rect when given); rows outside are left untouched. The stagger is a
   function of the row's top edge: `0.24 · (top − minTop) / (maxTop − minTop)`, zero when all tops
   are equal. Duration and ease unchanged. `overwrite: 'auto'` on `settle` and `flip`.
2. **Measure twice, not per frame.** `useRowMotion` returns a `mark()` that reads the row rects into
   its snapshot. The per-frame sampling effect goes. While `busy`, the layout effect returns without
   touching the snapshot. The Tasks view calls `mark()` inside `onGone` before it removes the id from
   `leaving`, so the snapshot holds the positions with the folded row at zero height and the
   following FLIP pass has nothing to move for rows that already slid up.
3. **Coalesced refresh.** In `app.tsx`, `onChanged` schedules `setRefresh` on an 80 ms trailing
   timer instead of calling it directly; the notify path (toast, conversation open) still runs at
   once. Each `load` in `Tasks.tsx`, `Home.tsx` and `Calendar.tsx` keeps a sequence ref: bump it
   before the fetch, and drop the response if the ref moved on.
4. **Home staggers capped.** Every `stagger: <number>` in `Home.tsx`'s scroll reveals becomes
   `stagger: { each, amount }` form with the total capped at 0.3 s, i.e. `each: min(each, 0.3 / n)`.
   Same shape as today when the list is short.

Reduced motion behaviour is unchanged: every helper still returns early under
`prefers-reduced-motion: reduce`.

### Testing
- A unit test for the stagger function (pure) in `motion-gsap.ts`: equal tops give 0, the lowest row
  gets 0.24, rows outside `within` are excluded.
- A test for the refresh coalescer: three calls within 80 ms yield one `setRefresh`.
- A test for the sequence guard: a response for request 1 arriving after request 2 is ignored.
- Manual: the audit harness (`scripts/_audit-shot.mjs`) with the real data's 34-row Later list; a
  view switch into Tasks should settle in under 0.6 s wall time.

## B. No zoom when a field is tapped on a phone

iOS zooms into a focused field whose text is under 16 px; the composer, jot and task-add fields are
0.92 rem (14.7 px on a phone, where `--u` is 1px).

- `web/src/styles.css`: under `@media (hover: none) and (pointer: coarse)`, every `input`,
  `textarea` and `select` gets `font-size: max(1rem, 16px)`. Desktop is untouched.
- `web/index.html`: the viewport meta gains `interactive-widget=resizes-content`.
- `styles.css`: `button, [role='button'], .tabs button, a { touch-action: manipulation; }`.
- No `maximum-scale`; pinch zoom stays available.

Testing: playwright at 390×844 with `hasTouch: true`, assert computed `font-size` of the composer
textarea ≥ 16px; assert the meta content contains `interactive-widget`.

## C. Sky: the theme follows the sun

A fourth choice next to System, Light and Dark. Light mode stays as it is and becomes Sky's day
keyframe.

### Colour model
Four keyframes, each a full token set (`sky-top`, `sky-mid`, `earth`, `haze`, `haze-strong`, `ink`,
`ivory`, `quiet`, `faint`, `line`, `track`, `sun-ink`, `sage`, `rose`), values as in
`mockups/sundial/sky.js` `K`:
- **day**: the current light set.
- **golden**: sun 0–8° over the horizon. Deeper blue top, amber mid, ground still light.
- **dusk**: civil and nautical twilight. Indigo top, ember mid, ground at 30% L; the dark text set.
- **night**: the current dark set, with the blue sky top the system-dark branch already uses.

Altitude → blend (`phaseAt`): ≥12° day; 4–12° golden→day; −2–4° dusk→golden; −10–−2° night→dusk;
below −10° night. Blend `t` is smoothstepped; `sky-top`, `sky-mid`, `earth` interpolate in oklch with
hue on the short arc. The remaining tokens are **stepped**, not blended: the light set (golden for
alt < 8°, else day) when alt ≥ 1°, the dark set (night for alt < −6°, else dusk) below. Stepping is
what keeps text contrast through twilight; the step is softened by registering those tokens with
`@property` and giving `:root[data-theme='sky']` a 400 ms transition on them.

### Sun position
`solarAltitude(date, lat, lon)` is the USNO low-precision algorithm in `sky.js`, verified against
Los Angeles on 2026-09-25 (sunrise 06:47, sunset 18:45; the function crosses 0° within two minutes
of each). The place is, in order:
1. `note.place` in localStorage, `{lat, lon}` rounded to 0.1°, set by "Use my location" in Settings
   through `navigator.geolocation.getCurrentPosition`. Never sent to the server.
2. Otherwise `placeFromZone(zone)`: latitude 35°, longitude = the zone's standard (non-DST) offset
   × 15°, standard being the smaller of the January and July offsets. Zone = the device's
   `Intl.DateTimeFormat().resolvedOptions().timeZone`.

### Application
- `web/src/sky.ts`: `solarAltitude`, `zoneOffsetHours`, `placeFromZone`, `phaseAt`, `paletteAt`,
  `applyPalette(root, alt)` lifted from `sky.js`, typed.
- `web/src/theme.ts`: `ThemeChoice` gains `'sky'`. `applyTheme('sky')` sets `data-theme="sky"`,
  writes the tokens inline on `:root`, sets `color-scheme` and the `theme-color` meta, caches
  `{tokens, dark}` under `note.sky`, and starts a 60 s interval (cleared when the choice changes)
  that also re-runs on `visibilitychange`. Other choices clear the inline tokens and the interval.
- `web/index.html` pre-paint: when `note.theme === 'sky'`, apply the cached `note.sky` tokens inline
  so a night-time first paint is already dark.
- `styles.css`: the `@media (prefers-color-scheme: dark)` branch takes the same `sky-top`/`sky-mid`
  as `[data-theme='dark']`, both set to the night keyframe. `@property` registrations for the stepped
  tokens; `:root[data-theme='sky'] { transition: --ink 400ms, ... }`.
- `Settings.tsx`: the Theme seg gains **Sky**. With Sky chosen the fold body shows a day strip (the
  palette across 24 h, sunrise and sunset ticks found by scanning altitude at 5-minute steps, a
  marker at now) and one line: "Following the sun over your time zone. Use my location for the
  exact sunrise and sunset. Your location stays on this device." With a stored place the line reads
  "Following the sun at your location." with **Forget my location**.

### Testing
- Unit: `solarAltitude` for Los Angeles at the four instants above; `phaseAt` boundaries; `paletteAt`
  returns the day set at 40° and the night set at −20°, and stepped tokens equal a keyframe exactly.
- Unit: `placeFromZone('America/Los_Angeles')` → `{lat: 35, lon: −120}`; `Asia/Tokyo` → `lon: 135`.
- Manual: the Settings strip and a screenshot at a forced altitude of −4° (dusk) via the audit harness.

## D. The day follows the device's time zone

- Server, `server/src/config.rs`: `UserConfig.timezone_auto: Option<bool>`, accessor defaulting to
  true. `server/src/api.rs`: settings GET returns `timezone_auto`; the PATCH accepts it. Nothing else
  on the server changes: day boundaries, nightly plan and close-the-day already read the saved zone.
- Client, `web/src/types.ts`: `Settings.timezone_auto: boolean`.
- Client, `web/src/app.tsx`: once `me` is set and again on every window `focus`, read the device zone;
  fetch settings; if `timezone_auto` and the device zone is in `settings.timezones` and differs from
  `settings.timezone`, PUT `{timezone: device}` and toast "Your day now follows Asia/Tokyo" with
  **Undo**, which PUTs `{timezone: previous, timezone_auto: false}`. At most one check per focus, and
  none while a check is in flight.
- `Settings.tsx`: under the Time zone row a **Follow this device** switch bound to `timezone_auto`.
  When on, the picker is disabled and shows the detected zone with the word "detected"; when off the
  picker works as it does today.

Testing: server test for the setting's default and round trip; a client unit test of the decision
function `zoneChange(settings, deviceZone) → {from, to} | null`.

## E. Begin working

### Server: the queue
`GET /api/tasks/queue?limit=5` returns the open work in the order the planner would lay it:
```json
[{ "task": Task, "step": Task | null, "planned_min": 30 | null, "reason": "overdue" }]
```
- Candidates: top-level tasks in `open`/`in_progress`, one entry per task, `step` = its first not-done
  child if any (the same choice as the Tasks view's `focusTarget`).
- Order: the `allocate::pack` key without `minutes`: in Now first, then `tasks::urgency_rank`
  ascending, then dated before undated, then due ascending, then created ascending, then id. Share
  the comparator with `allocate.rs` rather than copying it.
- `planned_min`: the step's duration, else the task's, rounded to 5; null when neither is set.
- `reason`, first that applies: `now` (in the Now group), `overdue` (due before now), `urgent`
  (urgency high), `due_soon` (pressing), `oldest`.
- Sharing: not exposed through share links.

`GET /api/sessions/today` returns `{ "focused_min": 52 }`: the sum of work time over sessions that
started today in the user's zone and have ended, minus their `paused_ms`, and for pomodoro sessions
minus break phases. The running session is not included; the client adds its own elapsed work time.

### Client: the face
Home shows **Begin working** when `session === null` and the day has no block on right now (`!next`
or `facts.wait` is non-empty). It is the first action in the row, styled `btn-sun` (background
`--sun`, text `--accent-fg`, the fill button's size and shadow). When a next block exists its
"Start" becomes **Start early** as a haze button; the overflow stays. Desktop hero, compact header
and mobile face all carry it.

Tapping it fetches the queue and enters `pick` state `{items, index}`. The face keeps the faded arc
and shows, in place of the wait: eyebrow "Up next", the task title (`gauge-name`), chips for
category, planned minutes and one of `overdue` (rose) / `urgent` (sun ink), a one-sentence reason,
and a row of dots for the candidates with the current one ringed. Reason copy:
- now: "You're already on it."
- overdue: "Due {relative day}, so it goes first."
- urgent: "Marked urgent."
- due_soon: "Due {weekday}, the soonest."
- oldest: "Nothing is due soon; this has waited longest."
If a following candidate exists, append " Then {its title}."

Actions: **Start · {planned} min** (or **Start** when null) calls `openNow` with the same fields the
Tasks view's `startFocus` builds (title, task_id, notes, step index/count/name, planned_min).
**Another** advances the index, wrapping. **Not now** below the row returns to the face. An empty
queue leaves the face and toasts "Nothing open to work on." Fetch failure toasts "Couldn't reach
Note." and stays.

Pomodoro is the server's decision from settings, as today; nothing is passed.

### Client: rounds
In a pomodoro session the face gains, under the name: a `dots` row with one dot per round up to
`max(4, round)`, rounds before the current one filled, the current one ringed; and beneath it
"{n} min focused today" from `GET /api/sessions/today` plus the running session's elapsed work time,
refreshed on `refresh`. Non-pomodoro sessions show only the focused line.

### Testing
- Server: queue order across the five reasons with fixture tasks; `planned_min` rounding; a shared
  task is not listed for a share token; `focused_min` sums two ended sessions and excludes the open one.
- Client: the reason sentence builder; the visibility rule (`session`, `next`, `facts.wait`) as a pure
  function; the pick's Another wraps.
- Manual: audit harness shots of idle, pick and round 2, phone and desktop.

## Out of scope
- Folding rows removed by another device (they still vanish at once).
- A quiet-window gate on Begin working.
- Sunrise and sunset in the Calendar.
