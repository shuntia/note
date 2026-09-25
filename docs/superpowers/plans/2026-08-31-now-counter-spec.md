# Spec: Now-screen elapsed counter with soft-drift digit transitions

Status: approved design, not yet implemented. Visual reference (source of truth for look and feel):
`scratchpad mockup: mockups/now-screen-dark.html` from the Daylight proposal session — the counter there
implements this spec exactly in vanilla JS. The Daylight proposal artifact shows the surrounding Now screen.

This spec is self-contained. Follow it literally; every value is exact and none are placeholders.

## 1. What is being built

A large time counter (`8:12` style) for the Now screen. Once per second the changed digit(s)
transition with a "soft drift": the old glyph rises ~a third of a digit-height, fading out through a
light blur, while the new glyph rises into place from just below, fading in through the same blur.
Unchanged digits must not move, re-render, or animate. The overall width of the counter must never
change during a tick (pseudo-monospace).

## 2. Exact constants

| Constant | Value | Notes |
|---|---|---|
| Digit cell width | `0.6em` | em-relative, so it scales with font size |
| Colon cell width | `0.32em` | |
| Drift distance | `0.38em` | up for the outgoing glyph, from below for the incoming |
| Peak blur | `5px` | fixed px, NOT em |
| Animation duration | `0.7s` (700 ms) | identical for in and out |
| Easing | `cubic-bezier(0.3, 0.6, 0.25, 1)` | identical for in and out |
| Tick interval | 1000 ms | drive from real clock, not accumulated setInterval drift (see §6.4) |
| Counter font | Fraunces, weight 420, `font-variant-numeric: tabular-nums` | Fraunces optical-size axis on; falls back Georgia, serif |

The two animations run simultaneously (both start on the same frame). Do not stagger them.

## 3. DOM contract (exact)

At rest, the counter element contains exactly one `.cell` per character, and each cell exactly one `.glyph`:

```html
<div class="now-counter" aria-live="off">
  <span class="cell"><span class="glyph">8</span></span>
  <span class="cell colon"><span class="glyph">:</span></span>
  <span class="cell"><span class="glyph">1</span></span>
  <span class="cell"><span class="glyph">2</span></span>
</div>
```

During a transition (≤ 700 ms), a changing cell additionally contains exactly one `.ghost`
(the outgoing character), absolutely positioned over the glyph:

```html
<span class="cell"><span class="glyph">3</span><span class="ghost">2</span></span>
```

The ghost is removed from the DOM on its `animationend`. A cell must never contain two ghosts.

Accessibility: `aria-live="off"` (a per-second live region is noise). Put a readable label on the
parent screen section instead (e.g. `aria-label="8 minutes 12 seconds elapsed of 25 minutes"`,
updated at most once per minute).

## 4. CSS (copy verbatim)

```css
.now-counter{
  font-family: "Fraunces", Georgia, serif;
  font-weight: 420;
  font-variant-numeric: tabular-nums;
  line-height: 1;
  /* font-size is set by the screen using it; the mockup uses 82px */
}
/* pseudo-monospace: fixed-width cells so layout never shifts when digits change */
.now-counter .cell{
  position: relative;
  display: inline-block;
  width: .6em;
  text-align: center;
}
.now-counter .cell.colon{ width: .32em }
.now-counter .glyph{ display: inline-block }

@keyframes nc-drift-in{
  0%  { transform: translateY(.38em); opacity: 0; filter: blur(5px) }
  100%{ transform: translateY(0);     opacity: 1; filter: blur(0) }
}
@keyframes nc-drift-out{
  0%  { transform: translateY(0);      opacity: 1; filter: blur(0) }
  100%{ transform: translateY(-.38em); opacity: 0; filter: blur(5px) }
}
.now-counter .glyph.chg{
  animation: nc-drift-in .7s cubic-bezier(.3,.6,.25,1);
}
.now-counter .ghost{
  position: absolute;
  inset: 0;
  pointer-events: none;
  animation: nc-drift-out .7s cubic-bezier(.3,.6,.25,1) forwards;
}
@media (prefers-reduced-motion: reduce){
  .now-counter .glyph.chg{ animation: none }
  .now-counter .ghost{ display: none }
}
```

Notes that are part of the spec, not commentary:
- `forwards` on the ghost only. The glyph animation has no fill mode (its resting style IS the end state).
- No `overflow: hidden` anywhere — the drift is small and the blur must be allowed to spill.
- No CSS `transition` properties on these elements; keyframe animations only.
- Do not animate `width`, `left/top`, or any layout property. Only `transform`, `opacity`, `filter`.

## 5. Update algorithm (follow step by step)

`render(counterEl, text)` where `text` is the formatted time (e.g. `"8:12"`):

1. If the number of `.cell` children differs from `text.length`:
   rebuild — replace ALL children with fresh cells (one glyph each, correct `colon` class,
   no `chg` classes, no ghosts) and RETURN. Rebuilds never animate. This covers `9:59 → 10:00`.
2. Otherwise, for each character index `i`:
   a. `glyph = cells[i].querySelector('.glyph')`.
   b. If `glyph.textContent === text[i]`: do nothing. Do not touch the cell at all.
   c. If different:
      1. Remove any existing `.ghost` in this cell (`cells[i].querySelector('.ghost')?.remove()`).
      2. Create `<span class="ghost">` with textContent = the OLD `glyph.textContent`.
      3. Add an `animationend` listener on the ghost that removes it (`ghost.remove()`).
      4. Append the ghost to the cell.
      5. Set `glyph.textContent = text[i]` (the NEW character).
      6. Restart the glyph animation, exactly like this:
         `glyph.classList.remove('chg'); void glyph.offsetWidth; glyph.classList.add('chg');`
         The `void glyph.offsetWidth` reflow is REQUIRED — without it, re-adding the class on
         consecutive ticks does not restart the animation.

Colon cells get `class="cell colon"` at build time (`char === ':'`). Nothing else ever
adds/removes the `colon` class.

## 6. Reference implementation (React 19, drop-in)

The app is React; React must NOT re-render the digits (re-rendering replaces text nodes and kills
running animations). The component renders an empty div once and drives it imperatively:

```tsx
import { useEffect, useRef } from "react";

function fmt(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function makeCell(c: string): HTMLSpanElement {
  const cell = document.createElement("span");
  cell.className = c === ":" ? "cell colon" : "cell";
  const glyph = document.createElement("span");
  glyph.className = "glyph";
  glyph.textContent = c;
  cell.append(glyph);
  return cell;
}

function render(el: HTMLElement, text: string, animate: boolean): void {
  const cells = el.children;
  if (cells.length !== text.length) {
    el.replaceChildren(...[...text].map(makeCell));
    return;
  }
  [...text].forEach((c, i) => {
    const cell = cells[i] as HTMLElement;
    const glyph = cell.querySelector(".glyph") as HTMLElement;
    if (glyph.textContent === c) return;
    if (animate) {
      cell.querySelector(".ghost")?.remove();
      const ghost = document.createElement("span");
      ghost.className = "ghost";
      ghost.textContent = glyph.textContent ?? "";
      ghost.addEventListener("animationend", () => ghost.remove());
      cell.append(ghost);
    }
    glyph.textContent = c;
    if (animate) {
      glyph.classList.remove("chg");
      void glyph.offsetWidth;
      glyph.classList.add("chg");
    }
  });
}

/**
 * startedAt: epoch ms when the session started.
 * durationSec: rough duration of the session (shown elsewhere; not used here).
 * mode: "elapsed" counts up from 0; "remaining" counts down from durationSec.
 *       Comes from the user's settings (default "elapsed").
 */
export function NowCounter({ startedAt, durationSec, mode }:
  { startedAt: number; durationSec: number; mode: "elapsed" | "remaining" }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const value = () => {
      const elapsed = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
      return mode === "remaining" ? Math.max(0, durationSec - elapsed) : elapsed;
    };
    let last = -1;
    const tick = (animate: boolean) => {
      const v = value();
      if (v === last) return;
      last = v;
      render(el, fmt(v), animate);
    };
    tick(false);                       // initial paint: never animate
    const id = setInterval(() => tick(!document.hidden), 250);
    const onVisible = () => { if (!document.hidden) tick(false); };  // catch-up: no animation
    document.addEventListener("visibilitychange", onVisible);
    return () => { clearInterval(id); document.removeEventListener("visibilitychange", onVisible); };
  }, [startedAt, durationSec, mode]);

  // key forces a clean rebuild when mode flips, so the flip never animates every digit
  return <div className="now-counter" aria-live="off" key={mode} ref={ref} />;
}
```

Design decisions embedded above — keep them:
- The interval runs at 250 ms but renders only when the derived second changes: seconds stay aligned
  to the real clock with no drift, and a suspended tab catches up cleanly.
- After a tab was hidden (`visibilitychange` → visible), the catch-up render passes `animate: false`,
  so a jump like `8:12 → 9:47` snaps rather than animating a garbage cascade.
- Flipping the elapsed/remaining setting remounts (key change) → rebuild, no animation storm.
- `startedAt` is a stable epoch timestamp; never pass `Date.now()` inline as a prop.

## 7. Settings

Add to Settings → Appearance (or a Now section): "Focus timer shows: Elapsed / Remaining".
Persist like the theme (`localStorage` key `note.nowCounter`, values `elapsed` | `remaining`,
default `elapsed`). This changes ONLY the number source (§6 `mode`); the arc, underline, and
transitions are identical in both modes.

## 8. Do NOT

- Do not animate cells whose character did not change (no whole-number pulses).
- Do not use CSS `transition` for this; keyframe animations only.
- Do not add `overflow: hidden` to cells (clips the blur).
- Do not let React re-render the cell DOM on tick (kills animations mid-flight).
- Do not skip the `void glyph.offsetWidth` reflow line.
- Do not attach the ghost before removing a previous ghost in the same cell.
- Do not animate on: initial mount, mode flip, length rebuild, or visibility catch-up.
- Do not change the constants in §2 without a design decision; they are tuned.

## 9. Acceptance checklist (verify each)

- [ ] At rest the DOM matches §3 exactly: N cells, one glyph each, zero ghosts, zero `chg` classes
      (the `chg` class may persist after animationend; acceptable — but no animation is running).
- [ ] During a normal tick, only the seconds cell mutates; other cells' DOM nodes are untouched
      (verify with a MutationObserver on a minutes cell across 10 ticks: zero records).
- [ ] `counterEl.getBoundingClientRect().width` is identical before, during, and after a tick.
- [ ] A cell never contains more than one `.ghost` (hammer-test: call render with alternating
      characters every 100 ms for 3 s, then assert).
- [ ] `9:59 → 10:00` produces a rebuild: cell count changes, no ghosts, no animation.
- [ ] With `prefers-reduced-motion: reduce`, digits swap instantly and no ghost is ever visible.
- [ ] Hide the tab for >10 s, return: the counter snaps to the correct value without animating.
- [ ] Toggling elapsed/remaining swaps the number instantly with no per-digit animation.
- [ ] The drift matches the mockup by eye: old digit rises and dissolves upward, new digit rises in
      from below, ~0.7 s, soft 5px blur at the extremes, no horizontal movement whatsoever.
