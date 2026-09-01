# Daylight Step 3 — Global Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put one always-present capture bar in the app shell so any thought becomes a Task in one keystroke and one Enter, from every view, without losing the current screen or an unsent draft.

**Architecture:** The capture bar lives in `web/src/app.tsx` — the shell already owns the sidebar, the mobile head, the toast, and `notify`/`onChanged`, which are exactly the four things capture needs. A new `Capture` component renders a pill-shaped form under the mobile head / above `<main>`, installs one `document` keydown listener for the `n` shortcut, mirrors its value into `localStorage` under `note.captureDraft`, and on submit *holds* the create for the 5 s undo window before calling `api.addTask`. No new file: capture is ~90 lines and belongs to the shell it mounts in, matching how `Today.tsx` keeps its held-drop next to the component that triggers it.

**Tech Stack:** React 19 + TypeScript, Vite, hand-written CSS with the existing token set in `web/src/styles.css`. No new dependencies.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — Step 3 (“Global capture”), plus the header bar in `docs/superpowers/mockups/daylight/today-desktop-light.html` and `tasks-desktop-light.html` (desktop) and `today-mobile-dark.html` (mobile pill).

## Global Constraints

- Design tokens: only tokens already in `web/src/styles.css` (`--sun`, `--sun-ink`, `--card`, `--mist`, `--sunk`, `--quiet`/`--text-muted`, `--ring`, `--radius*`). No new tokens, no new fonts — that is step 10. Accent = amber only. **Never introduce red.**
- Copy: sentence case, active voice, user vocabulary. Placeholder is **exactly** `Jot anything — a task, a thought, a change of plan`. No category picker, no options, no second field. Ever.
- Every state-changing action gets feedback within 100 ms and, where destructive, an in-place Undo. No confirm dialogs where undo will do.
- Accessibility floor: visible keyboard focus on all interactive elements; `prefers-reduced-motion` disables decorative animation; hit targets ≥ 40×40 px on touch layouts; text contrast ≥ 4.5:1.
- Client makes no external requests.
- Steps 1–2 shipped the toast-with-Undo mechanism (`notify(msg, { label, run })` in `web/src/app.tsx:39`). Step 3 **reuses** it. Do not build a second toast.
- Touch only `web/**` and this plan file. Another agent is editing `server/` concurrently: stage paths explicitly, never `git add -A`, never run `cargo test`.

---

## Why a held create (arguing from the spec)

Spec 3.3: “Submitting (Enter) … creates a Task … and shows toast `Saved to Tasks — Undo` (Undo deletes the just-created task).”

The server exposes exactly two task routes — `GET|POST /api/tasks` and `PATCH /api/tasks/{id}` (`server/src/api.rs:17-18`); `web/src/api.ts` mirrors them as `addTask` and `patchTask`. **There is no delete.** The two ways to honour “Undo removes it” without a Rust change:

1. `PATCH` the task to `state: 'dropped'`. Rejected: `dropped` is a real user state the agent reads; spending it on “this never happened” corrupts task history and leaves a row the user must look at.
2. **Hold the create.** The title is kept client-side, the toast shows immediately, and `api.addTask` fires only when the 5 s window closes with no Undo. This is the exact mechanism step 1 already uses for Drop (`web/src/views/Today.tsx:9-11,127-153`), so the codebase gains no second idea.

Option 2 is the choice. Consequence to accept knowingly: for 5 s the task is not on the server, so a Tasks view open in another tab will not show it yet. In this tab the wart is removed by having the commit call `onChanged()` and by making `Tasks` reload on `refresh` (Task 3) — today `Tasks` ignores `refresh` entirely (`web/src/views/Tasks.tsx:22`), so a captured task would not appear until the view remounted.

## Why the toast message is `Saved to Tasks`

Spec 3.3 quotes the toast as `Saved to Tasks — Undo`. The shell's toast renders a message span and, beside it, the action as a pill button with a `0.9rem` gap (`web/src/styles.css:1619-1647`). Passing the full quoted string as the message renders **Saved to Tasks — Undo · [Undo]** — the word twice. The quoted string describes the whole toast, the trailing `— Undo` being its button, exactly as in step 1's `Dropped "<name>" — moved off today` **with an Undo button**. So the message is `Saved to Tasks` and the button is `Undo`; on screen it reads `Saved to Tasks   Undo`.

## Why the shell, not a per-view component

Spec 3.1 says the bar “sits in the app header on every view” and 3.5 says the draft “survives view switches”. `App` is the only component that outlives a view switch (`web/src/app.tsx:104-110` swaps the whole `<main>` body). Mounting capture there gives draft survival for free and puts the keydown listener where no view can unmount it. The mockups agree: the pill is in the chrome, outside the scrolling column (`today-desktop-light.html` `header > .capture`; `today-mobile-dark.html` `header > .capture`).

The desktop mockup's header also carries a right-aligned date. The sidebar already prints `username · date` (`web/src/app.tsx:79-81`), so repeating it is out of scope for this step; the header carries capture only.

## File Structure

- `web/src/app.tsx` — **modify.** Add the `Capture` component (module-scoped hold + the `n` listener + draft persistence) and mount it between `.mobile-head` and `<main>`.
- `web/src/views/Tasks.tsx` — **modify, one line.** Reload on `refresh` so a committed capture appears.
- `web/src/styles.css` — **modify.** Add the `.capture-*` block: sticky wrapper, pill, plus glyph, `N` chip, focus ring, touch sizing, reduced-motion.

No test files: this repo's `web/` has no test harness (`web/package.json` has no test script and no runner). Verification is `tsc --noEmit`, `vite build`, and scripted browser checks against a stub API, spelled out in Task 4.

---

### Task 1: The capture bar in the shell

**Files:**
- Modify: `web/src/app.tsx` (imports at :1, new component after `App`, mount at :96-103)
- Modify: `web/src/styles.css` (new block after the `:is(.chat-composer, .quick-add)` rules, ~:335)

**Interfaces:**
- Consumes: `notify(msg, action?)` and `onChanged()` from `App` (`web/src/app.tsx:39,45`); `api.addTask(title) => Promise<Task>` (`web/src/api.ts:77`).
- Produces: `function Capture({ notify, onChanged }: { notify: (msg: string, action?: ToastAction) => void; onChanged: () => void })`, rendered by `App`. Nothing else imports it.

- [ ] **Step 1: Add the module constants and the hold slot in `web/src/app.tsx`**

Place directly under the existing `type Tab = …` line:

```tsx
const DRAFT_KEY = 'note.captureDraft'
const CAPTURE_PLACEHOLDER = 'Jot anything — a task, a thought, a change of plan'
const UNDO_MS = 5000

// No API removes a task, so the create waits out the undo window before it is sent.
let heldCapture: { title: string; timer: number } | null = null
```

- [ ] **Step 2: Write the `Capture` component**

Append after the `App` function (before `Login`):

```tsx
function readDraft(): string {
  try {
    return localStorage.getItem(DRAFT_KEY) ?? ''
  } catch {
    return ''
  }
}

function writeDraft(text: string) {
  try {
    if (text) localStorage.setItem(DRAFT_KEY, text)
    else localStorage.removeItem(DRAFT_KEY)
  } catch {
    // storage blocked; the draft still holds for this session
  }
}

function Capture({
  notify,
  onChanged,
}: {
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
}) {
  const [text, setText] = useState(readDraft)
  const input = useRef<HTMLInputElement>(null)
  // Where focus was when the shortcut stole it, so Esc can hand it back.
  const returnTo = useRef<HTMLElement | null>(null)

  const commit = useCallback(() => {
    if (!heldCapture) return
    const { title, timer } = heldCapture
    heldCapture = null
    window.clearTimeout(timer)
    api
      .addTask(title)
      .then(onChanged)
      .catch(() => notify("Couldn't save that. Try again."))
  }, [notify, onChanged])

  useEffect(() => commit, [commit])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'n' || e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return
      const el = e.target as HTMLElement | null
      const tag = el?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el?.isContentEditable) return
      e.preventDefault()
      returnTo.current = el
      input.current?.focus()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const change = (value: string) => {
    setText(value)
    writeDraft(value)
  }

  const submit = (e: FormEvent) => {
    e.preventDefault()
    const title = text.trim()
    if (!title) return
    change('')
    commit()
    heldCapture = { title, timer: window.setTimeout(commit, UNDO_MS) }
    notify('Saved to Tasks', {
      label: 'Undo',
      run: () => {
        if (heldCapture?.title !== title) return
        window.clearTimeout(heldCapture.timer)
        heldCapture = null
      },
    })
  }

  return (
    <form className="capture" onSubmit={submit}>
      <span className="capture-glyph" aria-hidden="true">
        +
      </span>
      <input
        ref={input}
        value={text}
        aria-label={CAPTURE_PLACEHOLDER}
        placeholder={CAPTURE_PLACEHOLDER}
        onChange={(e) => change(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== 'Escape') return
          e.currentTarget.blur()
          returnTo.current?.focus()
          returnTo.current = null
        }}
      />
      <kbd className="capture-key" aria-hidden="true">
        N
      </kbd>
    </form>
  )
}
```

- [ ] **Step 3: Mount it in the shell**

In `App`'s `.content` div, between the mobile head and `<main>`:

```tsx
        <Capture notify={notify} onChanged={onChanged} />
```

The `FormEvent` type import already exists on line 1 of the file; `useCallback`, `useEffect`, `useRef`, `useState` are already imported too. Verify no import edit is needed.

- [ ] **Step 4: Style it in `web/src/styles.css`**

Add after the shared-composer block (~line 335). Tokens only; the pill mirrors `.capture` in `today-desktop-light.html` (card fill, mist border, 999px radius, sun-ink glyph, sunk `kbd`):

```css
/* global capture: the shell's one always-there input */
.capture {
  flex: none;
  position: sticky;
  top: 0;
  z-index: 5;
  display: flex;
  align-items: center;
  gap: 0.6rem;
  width: 100%;
  max-width: 32.5rem;
  margin: 1.1rem 1.5rem 0;
  padding: 0.5rem 1rem;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 999px;
  transition: border-color 150ms ease, box-shadow 150ms ease;
}
.capture:focus-within {
  border-color: var(--ring);
  box-shadow: 0 0 0 1px var(--ring);
}
.capture-glyph {
  flex: none;
  color: var(--sun-ink);
  font-size: 1.05rem;
  line-height: 1;
}
.capture input {
  flex: 1;
  min-width: 0;
  padding: 0.15rem 0;
  background: none;
  border: none;
  border-radius: 0;
  font-size: 0.95rem;
}
.capture input:focus-visible { outline: none; }
.capture-key {
  flex: none;
  font: inherit;
  font-size: 0.7rem;
  color: var(--text-muted);
  background: var(--sunk);
  border: 1px solid var(--border);
  border-radius: 5px;
  padding: 0.05rem 0.35rem;
}
@media (prefers-reduced-motion: reduce) {
  .capture { transition: none; }
}
```

Then the shell placement — desktop puts the pill in its own header strip, mobile sits it under the existing mobile head and drops the key chip (the mockup's mobile pill has none). Add to the existing `@media (max-width: 767.98px)` shell block at ~line 211-226:

```css
  .capture { margin: 0 1rem 0.35rem; width: auto; min-height: 2.5rem; }
  .capture-key { display: none; }
```

The desktop margin is `1.1rem 1.5rem 0`, not `auto`: the mockup's header left-aligns the pill against the main column's padding edge while the reading column below centres itself (`today-desktop-light.html` `header{padding:14px 28px}` vs `.col{margin:0 auto}`).

- [ ] **Step 5: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add web/src/app.tsx web/src/styles.css
git commit -m "feat: global capture bar in the app shell"
```

---

### Task 2: Draft survives reload and view switches

**Files:**
- Modify: none beyond Task 1 — this task is the verification gate for spec 3.5.

**Interfaces:**
- Consumes: `readDraft`/`writeDraft` from Task 1.
- Produces: nothing.

- [ ] **Step 1: Confirm the initial state reads storage synchronously**

`useState(readDraft)` (lazy initializer, not `useState(readDraft())`) means the first paint already carries the draft — no flash of empty bar. Confirm the code says `useState(readDraft)`.

- [ ] **Step 2: Confirm submit clears storage before the hold**

In `submit`, `change('')` runs before `heldCapture` is set, so the key is removed on submit even if the create later fails. Spec 3.5: “cleared on submit”. Confirm ordering.

- [ ] **Step 3: Confirm view switches cannot clear it**

`Capture` is a child of `App`, not of `<main>`'s conditional tree (`web/src/app.tsx:104-110`), so `setTab` never unmounts it. Confirm the mount point is outside `<main>`.

---

### Task 3: A captured task shows up in Tasks

**Files:**
- Modify: `web/src/views/Tasks.tsx:22`

**Interfaces:**
- Consumes: `refresh` from `ViewProps` (`web/src/app.tsx:16-20`), already destructurable.
- Produces: nothing.

- [ ] **Step 1: Make the view refetch when the shell bumps `refresh`**

Replace the props destructure and the load effect:

```tsx
export function Tasks({ notify, refresh }: ViewProps) {
```

```tsx
  useEffect(load, [refresh])
```

- [ ] **Step 2: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: no output. (`load` is a plain function redefined per render; the dependency is deliberately `refresh` alone, matching the existing mount-once behaviour plus shell nudges.)

- [ ] **Step 3: Commit**

```bash
git add web/src/views/Tasks.tsx
git commit -m "fix: tasks view reloads on shell refresh"
```

---

### Task 4: Verify against the acceptance checklist

**Files:**
- Create (scratch, not committed): a stub API server and a CDP driver script under the session scratchpad.

**Interfaces:**
- Consumes: the built app.
- Produces: pass/fail evidence for the three spec 3 acceptance items.

- [ ] **Step 1: Build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed; paste the real output into the report.

- [ ] **Step 2: Serve the built bundle behind a stub API**

Write a Node script that serves `web/dist` and answers the routes the shell touches on boot — `GET /api/me` → `{"username":"tester","admin":false}`, `GET /api/plan/today` → `[]`, `GET /api/debrief` → 404, `GET /api/tasks` → the in-memory list, `POST /api/tasks` → append and echo, `GET /api/conversations` → `[]`, `GET /api/settings` → a minimal object. Keep the created-task list readable over a side route so the driver can assert on it.

- [ ] **Step 3: Drive Chromium over CDP**

`chromium --headless --remote-debugging-port=…`; Node 24 has a global `WebSocket`, so the driver needs no dependency. Assert, in order:

1. For each of the five tabs: dispatch a `keydown` for `n` at `document`, then read `document.activeElement.className` — expect `capture`'s input each time. Then focus Talk's `textarea`, dispatch `n`, expect `activeElement` to stay the textarea and its value to receive the character.
2. Type a title, submit the form, expect the toast text `Saved to Tasks — Undo` within 100 ms; click `.toast-action`; wait past 5 s; expect the stub's task list to still be empty. Repeat without clicking Undo; after 5 s expect the title present in the stub's list and rendered in the Tasks view.
3. Type text, switch tabs, reload the page, expect the input's value unchanged and `localStorage['note.captureDraft']` to match.

- [ ] **Step 4: Record results**

State pass/fail per acceptance line with the observed value as evidence. If the browser cannot be driven, say so plainly rather than claiming visual verification.

---

## Self-Review

**Spec coverage.**
- 3.1 capture bar in the header on every view — Task 1 Step 3 (mounted in `App`, outside `<main>`); mobile pill at the top — Task 1 Step 4 mobile block.
- 3.2 `n` focuses, except in input/textarea/contenteditable; `Esc` blurs and restores — Task 1 Step 2 (`onKey` guard + `returnTo`). `SELECT` is guarded too: typing `n` in a native select jumps its options, which the shortcut must not steal.
- 3.3 Enter creates a Task, clears the bar, toast `Saved to Tasks — Undo`, user stays put — Task 1 Step 2 `submit`; the held create is argued above; no navigation happens anywhere in the component.
- 3.4 exact placeholder, one field — `CAPTURE_PLACEHOLDER`, single `<input>`.
- 3.5 draft survives switches and reload under `note.captureDraft`, cleared on submit — Tasks 1 and 2.
- Acceptance lines — Task 4.

**Placeholder scan.** No TBDs; every code step carries the literal code.

**Type consistency.** `Capture` takes `notify`/`onChanged` with the signatures `App` already has; `heldCapture` is `{ title: string; timer: number }` at declaration and at every use; `ToastAction` is the exported type from the same file.

**Not in this step.** The desktop header's date line (the sidebar already prints it), any Tasks grouping (step 4), tokens/fonts (step 10).
