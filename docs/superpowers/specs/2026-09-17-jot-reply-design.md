# Jot anything: a one-shot message to Note

Date: 2026-09-17. Status: approved for implementation (autonomous session).

## Goal

The "Jot anything" box sends its text to Note as a chat message instead of
filing a task directly. Note's reply opens in a panel right under the box.
Clicking away closes it; clicking the reply opens the chat view on that
conversation to keep talking. Every transition is animated so the box and the
panel read as one object.

## Behaviour

- One component, `web/src/jot.tsx` (`Jot`), replaces both the topbar
  `Capture` in `app.tsx` and the `TellNote` on the mobile Home. `tellnote.tsx`
  and its `lastConversation` storage are deleted; the Talk view keeps its own
  composer. The `N` shortcut, draft persistence (`note.captureDraft`) and Esc
  handling move over unchanged.
- Submit → the input clears, the panel unfolds under the box showing the sent
  text (muted, right-aligned) and a "Note is thinking…" line that follows the
  live receipts (`onAgentFrame` from `ws.ts`, filtered to frames whose
  `conversation_id` matches the panel's conversation or is `null` while the
  first message has none; `receipts.doing()` gives the present-tense text).
  The reply arrives via `api.talk(text, conversationId)` and renders as
  sanitised markdown (`markdown.tsx`). Errors show "Couldn't reach Note. Try
  again." in the panel and keep the text in the input.
- While the panel is open, another jot continues the **same** conversation
  and appends below (the panel is a short thread). Once closed, the next jot
  starts a fresh conversation. Nothing is written until a message is sent.
- Close: a pointerdown outside the box+panel, Esc in the box, or the shell
  changing tabs. Open the thread: click or Enter on the panel body →
  `openConversation(id)` (already in `app.tsx`) and the panel closes. A
  panel with no conversation yet (still in flight on the first message) opens
  the chat with the draft prefilled instead (`openTalk`).
- Task capture still works because the persona's tools include `task_create`;
  the receipt line reads "Adding a task…" and the reply confirms. No
  client-side `addTask` from the box.

## Motion

`web/src/motion-gsap.ts` gains `unfold(el)` / `fold(el, done)`: height from
0 to auto and `autoAlpha`, 0.28 s `expo.out` in, 0.2 s `power2.in` out, with
the box's bottom corners squaring off (`.jot.open` toggles `border-radius`
through a 180 ms CSS transition) so the panel grows out of the box. New
messages inside an open panel use `settle`. Reduced motion falls back to
instant show/hide, as the other helpers do. The topbar panel is
`position: absolute` under the box (max-height 60vh, scrolls) so the layout
underneath does not jump; the mobile Home panel is in flow, directly under
the box, and the page scrolls with it.

## Styling

`.jot` keeps the `.capture` pill look; `.jot-panel` is a `--haze-strong` card
with `--radius-lg` bottom corners, a 1px `--line` top rule, the user's line in
`--faint`, Note's reply in `--ink` with markdown spacing from the Talk view,
the thinking line in `--sun-ink` with the pulse dot from `pulse.tsx`. Hover on
the reply shows a trailing "Open in Chat ›" hint in `--faint`.

## Testing

`pnpm build` clean; a manual pass in a headless browser (the `run` skill)
checking: submit opens the panel, receipt updates, reply renders, click-away
closes, click opens Chat on the same conversation, mobile Home behaves the
same, reduced motion shows the panel without animation.
