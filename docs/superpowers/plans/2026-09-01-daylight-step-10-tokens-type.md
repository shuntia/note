# Daylight Step 10 — Tokens, Type, and Theme Pass: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the nine shipped Daylight steps behind one token set taken from the mockups, two self-hosted OFL typefaces, the gradient spine, the mocked sidebar/tab treatment, and headings with no diamond markers.

**Architecture:** `web/src/styles.css` keeps its existing class vocabulary; only the `:root` blocks and the rules that carry colour/type change. The mockups' names (`--dawn --card --sunk --ink --quiet --faint --mist --sun --sun-ink --moss --clay`) become canonical, and the older names (`--bg --surface --text --text-muted --border --border-input --accent`) stay defined as aliases so the ~1900 lines of shipped rules resolve without a sweeping rewrite. Fonts ship as subsetted woff2 under `web/src/fonts/`, referenced by relative `url()` so Vite fingerprints and inlines them into `dist/` — no external host is ever contacted.

**Tech Stack:** Vite 7 + React 19 + TypeScript; plain CSS with custom properties, `oklch()` and `color-mix()`; fontTools/pyftsubset + brotli for the font pipeline (scratch venv, not a project dependency).

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` (step 10, plus "Global constraints"). Companion: `docs/superpowers/plans/2026-08-31-now-counter-spec.md`. Visual truth: `docs/superpowers/mockups/daylight/*.html`.

## Global Constraints

- Design tokens: accent = amber only; moss = done; clay = dropped/warn. **Never introduce red.**
- Copy rules: sentence case; active voice; an action keeps its name through its whole flow; user vocabulary, never system vocabulary. **No copy string changes in this step.**
- Accessibility floor: visible keyboard focus on all interactive elements; `prefers-reduced-motion` disables all decorative animation; hit targets ≥ 40×40 px on touch layouts; **text contrast ≥ 4.5:1**.
- All assets bundled; the client makes **no external requests** (fonts included).
- Mockup wins on look, spec wins on behavior — but the global constraints bind over both.
- Do not regress steps 1–9: the counter's exact geometry, the Now screen's entry beats, and every copy string stay as shipped.

---

## The contrast conflict, resolved

Measured with WCAG 2.x relative luminance after an oklch → sRGB conversion, against the three mockup surfaces (`--dawn`, `--card`, `--sunk`):

| token | light value | worst ratio | dark value | worst ratio |
|---|---|---|---|---|
| `--ink` | `oklch(24% 0.02 60)` | 14.43 | `oklch(93% 0.008 80)` | 13.04 |
| `--quiet` | `oklch(48% 0.015 60)` | 5.74 | `oklch(72% 0.012 70)` | 6.45 |
| `--faint` **as mocked** | `oklch(62% 0.012 60)` | **3.19** ✗ | `oklch(56% 0.012 70)` | **3.44** ✗ |
| `--sun-ink` | `#9c5c05` | 4.65 | `#edaa4e` | 7.99 |
| `--moss` | `oklch(52% 0.09 145)` | 4.63 | `oklch(72% 0.09 145)` | 6.69 |
| `--clay` | `oklch(52% 0.06 40)` | 4.94 | `oklch(68% 0.07 40)` | 5.42 |
| `--sun` **as text** | `#e8871e` | **2.32** ✗ | `#e8871e` | 6.04 |

Two breaches, two fixes, both stated in the CSS:

1. **`--faint`** is moved to the floor: light `oklch(53% 0.012 60)` (4.63 worst) and dark `oklch(63% 0.012 70)` (4.57 worst). This is the smallest shift that clears 4.5:1 on all three surfaces. With a compliant third grey in hand, the screen-scoped `--now-faint` `color-mix` in `.now-screen` is retired and the six sites whose step comments recorded the collapse of the third grey get `var(--faint)` back.
2. **`--sun` is never a text colour in light mode.** Every `color: var(--accent)` / `color: var(--sun)` site becomes `var(--sun-ink)`, or takes the mockups' sunk-background active treatment.

Two knock-on rules follow from the same measurement, and both happen to be what the mockups already do:

- Category chips lose their tinted fill (any fill eats sun-ink's 4.95 margin down to ~4.3). `memory-desktop-light.html` draws them outline-only: `color: var(--cat)` on a `color-mix(cat 30%, mist)` border. Adopt that.
- Active/selected rows lose their `color-mix(accent 15%, transparent)` fill for `background: var(--sunk); color: var(--ink)` — `nav a.active` and `.convos .item.active` in the mockups.

`--danger` (a pre-Daylight red) is the last red in the sheet and is used only by error affordances. Those move to `--clay` — the vocabulary step 8 already established for a failed tool call — and the token is removed rather than left orphaned.

`--mist` is a hairline, not text, and carries no contrast floor.

---

## File Structure

- `web/src/fonts/` — **created.** Four subsetted woff2 files, two OFL licence texts, and a README recording the upstream URLs and the exact regeneration commands. Self-contained; nothing else in the tree writes here.
- `web/src/styles.css` — **modified.** `@font-face` block, both `:root` blocks rewritten, and the rules carrying colour/type updated. Everything in this step that is not markup lives here.
- `web/src/app.tsx` — **modified.** Nav icons in the sidebar and the mobile tabs; the wordmark diamond becomes the mocked sun disc.
- `web/src/navicon.tsx` — **created.** The five nav glyphs, transcribed from the mockups, as one small component. Kept out of `app.tsx` so the shell stays readable, mirroring `bell.tsx`.
- `web/src/section.tsx` — **modified.** The `◆` marker before section headings is deleted.
- `web/src/views/Talk.tsx` — **modified.** The empty-state diamond becomes a disc.
- `web/src/views/Now.tsx` — **unmodified.** The `--now-faint` retirement is entirely inside `styles.css`.
- `web/index.html` — **modified.** The static `theme-color` fallback matches the new light `--dawn`.

**Fraunces' five roles, and their selectors.** Everything else that resolves `--serif` today moves to `--sans`; the token is renamed `--display` so the constraint is legible at the point of use.

| role | selectors |
|---|---|
| wordmark | `.brand`, `.now-brand`, `.login h1` |
| view titles | `.pane-title` (the `SectionTitle` component, both as a view title and as a Settings pane heading — the mockups set `h1.view` and `.pane h2` in serif) |
| Now card title | `.nowcard-title` |
| Now-screen counter | `.now-counter` |
| Memory detail titles | `.memory-title` |

Losing serif: `.today-clear`, `.chat-head h2`, `.chat-empty`, `.turn.pending`, `.prose` (+ its headings), `.memory-blank`, `.dialog-title`, `.letter`. `chat-desktop-dark.html` sets assistant prose in `--sans`; `today-*.html` sets the debrief letter in `--sans`. This is the mockups' own hierarchy, not a reduction of it.

---

### Task 1: The font bundle

**Files:**
- Create: `web/src/fonts/fraunces-subset.woff2`, `web/src/fonts/atkinson-400.woff2`, `web/src/fonts/atkinson-700.woff2`, `web/src/fonts/atkinson-400-italic.woff2`
- Create: `web/src/fonts/Fraunces-OFL.txt`, `web/src/fonts/AtkinsonHyperlegible-OFL.txt`, `web/src/fonts/README.md`
- Test: manual — `fontTools` inspection of the outputs, then the browser check in Task 7

**Interfaces:**
- Consumes: nothing.
- Produces: the four woff2 paths above, referenced by Task 2's `@font-face` block as `url('./fonts/<name>.woff2')`. Fraunces exposes a live `opsz` axis and a `wght` axis limited to 420–560, so its `@font-face` declares `font-weight: 420 560`.

- [ ] **Step 1: Build a scratch venv**

`fontTools` is not installed and must not become a project dependency.

```bash
uv venv "$SCRATCH/fontenv"
uv pip install --python "$SCRATCH/fontenv" fonttools brotli
```

- [ ] **Step 2: Fetch the upstream OFL binaries and licences**

```bash
BASE=https://raw.githubusercontent.com/google/fonts/main/ofl
curl -fsSL -o "$SCRATCH/Fraunces.ttf" "$BASE/fraunces/Fraunces%5BSOFT%2CWONK%2Copsz%2Cwght%5D.ttf"
curl -fsSL -o "$SCRATCH/Fraunces-OFL.txt" "$BASE/fraunces/OFL.txt"
for f in Regular Bold Italic; do
  curl -fsSL -o "$SCRATCH/Atkinson-$f.ttf" "$BASE/atkinsonhyperlegible/AtkinsonHyperlegible-$f.ttf"
done
curl -fsSL -o "$SCRATCH/Atkinson-OFL.txt" "$BASE/atkinsonhyperlegible/OFL.txt"
```

Verify each `.ttf` starts with a valid sfnt magic and is tens-to-hundreds of KB — a 404 arrives as an HTML page of plausible size.

- [ ] **Step 3: Partial-instance Fraunces**

`SOFT` and `WONK` are pinned to their defaults; `opsz` stays live so `font-optical-sizing: auto` works; `wght` is clipped to the two weights the design uses.

```bash
"$SCRATCH/fontenv/bin/fonttools" varLib.instancer "$SCRATCH/Fraunces.ttf" \
  SOFT=0 WONK=0 wght=420:560 -o "$SCRATCH/fraunces-partial.ttf"
```

- [ ] **Step 4: Subset all four faces to woff2**

The unicode set is Latin plus exactly the punctuation and symbols the UI renders (`— · … ✓ ± ▾ ≈ – “ ” ⋯ § ← ✕ ▸ ° ＋ ▶ − ☀`), widened to Latin-1 Supplement and Latin Extended-A because task titles, event names and memory summaries are user and agent text.

```bash
U='U+0020-007E,U+00A0,U+00A7,U+00B0,U+00B1,U+00B7,U+00D7,U+00C0-00FF,U+0100-017F,\
U+2010-2015,U+2018,U+2019,U+201C,U+201D,U+2026,U+2039,U+203A,U+2190,U+2212,U+2248,\
U+2260,U+2264,U+2265,U+22EF,U+25B4,U+25B6,U+25B8,U+25BE,U+2600,U+2713,U+2715,U+FE0E,U+FF0B'
FEAT='--layout-features+=tnum,kern,liga,clig,onum,lnum'
"$SCRATCH/fontenv/bin/pyftsubset" "$SCRATCH/fraunces-partial.ttf" --unicodes="$U" $FEAT \
  --flavor=woff2 --output-file=web/src/fonts/fraunces-subset.woff2
"$SCRATCH/fontenv/bin/pyftsubset" "$SCRATCH/Atkinson-Regular.ttf" --unicodes="$U" $FEAT \
  --flavor=woff2 --output-file=web/src/fonts/atkinson-400.woff2
"$SCRATCH/fontenv/bin/pyftsubset" "$SCRATCH/Atkinson-Bold.ttf" --unicodes="$U" $FEAT \
  --flavor=woff2 --output-file=web/src/fonts/atkinson-700.woff2
"$SCRATCH/fontenv/bin/pyftsubset" "$SCRATCH/Atkinson-Italic.ttf" --unicodes="$U" $FEAT \
  --flavor=woff2 --output-file=web/src/fonts/atkinson-400-italic.woff2
```

- [ ] **Step 5: Verify the outputs carry what the CSS asks of them**

`tnum` must survive in every face — `font-variant-numeric: tabular-nums` is on the Now-screen counter, the time gutter, the block ranges and the ±15 chips. Fraunces must still have `fvar` with `opsz` live and `wght` 420–560.

```python
from fontTools.ttLib import TTFont
for p in ["fraunces-subset", "atkinson-400", "atkinson-700", "atkinson-400-italic"]:
    f = TTFont(f"web/src/fonts/{p}.woff2")
    feats = {r.FeatureTag for r in f["GSUB"].table.FeatureList.FeatureRecord}
    print(p, "tnum" in feats, [(a.axisTag, a.minValue, a.maxValue) for a in f["fvar"].axes] if "fvar" in f else "static")
```

Expected: `tnum` True for all four; Fraunces `[('opsz', 9, 144), ('wght', 420, 560)]`.

- [ ] **Step 6: Copy the licences and write the README**

```bash
cp "$SCRATCH/Fraunces-OFL.txt" web/src/fonts/Fraunces-OFL.txt
cp "$SCRATCH/Atkinson-OFL.txt" web/src/fonts/AtkinsonHyperlegible-OFL.txt
```

`web/src/fonts/README.md` records, for each family: what the file is, its upstream URL, the exact commands above, and the licence file it ships under. Terse and factual.

---

### Task 2: The token set and the type stack

**Files:**
- Modify: `web/src/styles.css:1-87` (both `:root` blocks, the `@font-face` block, `body`)
- Modify: `web/index.html:6` (the static `theme-color` fallback)

**Interfaces:**
- Consumes: Task 1's four woff2 paths.
- Produces: the canonical token names `--dawn --card --sunk --ink --quiet --faint --mist --mist-strong --sun --sun-ink --moss --clay --accent-fg --spine --display --sans --mono`, and the aliases `--bg --surface --surface-2 --text --text-muted --border --border-input --accent --ring --sidebar-bg --sidebar-border --bubble --code-bg` that Tasks 3–6 leave in place. `--serif` and `--danger` no longer exist.

- [ ] **Step 1: Add the `@font-face` block at the top of the sheet**

```css
@font-face {
  font-family: 'Atkinson Hyperlegible';
  src: url('./fonts/atkinson-400.woff2') format('woff2');
  font-weight: 400;
  font-style: normal;
  font-display: swap;
}
@font-face {
  font-family: 'Atkinson Hyperlegible';
  src: url('./fonts/atkinson-700.woff2') format('woff2');
  font-weight: 700;
  font-style: normal;
  font-display: swap;
}
@font-face {
  font-family: 'Atkinson Hyperlegible';
  src: url('./fonts/atkinson-400-italic.woff2') format('woff2');
  font-weight: 400;
  font-style: italic;
  font-display: swap;
}
/* variable: opsz stays live, wght is clipped to the two the design uses */
@font-face {
  font-family: 'Fraunces';
  src: url('./fonts/fraunces-subset.woff2') format('woff2');
  font-weight: 420 560;
  font-style: normal;
  font-display: swap;
}
```

- [ ] **Step 2: Replace the light `:root` block**

```css
:root {
  color-scheme: light;

  --dawn: oklch(97.5% 0.006 85);
  --card: oklch(99.2% 0.004 85);
  --sunk: oklch(95.4% 0.008 80);

  --ink: oklch(24% 0.02 60);
  --quiet: oklch(48% 0.015 60);
  /* the mockups' third grey measures 3.4:1 on paper; this is the floor above it */
  --faint: oklch(53% 0.012 60);

  --mist: oklch(89% 0.009 75);
  --mist-strong: oklch(83% 0.011 75);

  /* --sun fills; --sun-ink is the same accent at a weight text can be set in */
  --sun: #e8871e;
  --sun-ink: #9c5c05;
  --moss: oklch(52% 0.09 145);
  --clay: oklch(52% 0.06 40);
  --accent-fg: oklch(20% 0.03 60);

  /* the shape of a day, drawn once down Today's spine; decorative only */
  --spine: linear-gradient(
    to bottom,
    oklch(80% 0.07 80),
    oklch(84% 0.02 90) 38%,
    oklch(78% 0.05 300) 78%,
    oklch(60% 0.04 285)
  );

  --radius: 0.625rem;
  --radius-lg: 0.875rem;
  --radius-xl: 1.25rem;

  --display: 'Fraunces', Georgia, 'Iowan Old Style', serif;
  --sans: 'Atkinson Hyperlegible', system-ui, -apple-system, 'Segoe UI', sans-serif;
  --mono: ui-monospace, 'SF Mono', Menlo, monospace;

  /* the names the shipped rules resolve through */
  --bg: var(--dawn);
  --surface: var(--card);
  --surface-2: var(--card);
  --text: var(--ink);
  --text-muted: var(--quiet);
  --border: var(--mist);
  --border-input: var(--mist-strong);
  --accent: var(--sun);
  --ring: var(--sun-ink);
  --sidebar-bg: color-mix(in oklch, var(--dawn) 70%, var(--card));
  --sidebar-border: var(--mist);
  --bubble: var(--sunk);
  --code-bg: var(--sunk);
}
```

- [ ] **Step 3: Replace both dark blocks with one shared declaration list**

Only the tokens that differ are redeclared; every derived alias follows on its own.

```css
:root[data-theme='dark'] {
  color-scheme: dark;
  --dawn: oklch(17.5% 0.01 60);
  --card: oklch(21.5% 0.012 60);
  --sunk: oklch(25% 0.014 65);
  --surface-2: var(--sunk);
  --ink: oklch(93% 0.008 80);
  --quiet: oklch(72% 0.012 70);
  /* the mockups' third grey measures 4.1:1 here; this is the floor above it */
  --faint: oklch(63% 0.012 70);
  --mist: oklch(30% 0.012 65);
  --mist-strong: oklch(38% 0.014 65);
  --sun-ink: #edaa4e;
  --moss: oklch(72% 0.09 145);
  --clay: oklch(68% 0.07 40);
  --spine: linear-gradient(
    to bottom,
    oklch(55% 0.07 80),
    oklch(45% 0.02 90) 38%,
    oklch(48% 0.06 300) 78%,
    oklch(40% 0.05 285)
  );
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme='light']) {
    /* …the identical declaration list… */
  }
}
```

- [ ] **Step 4: Point `body` at the new stack and update the html fallback**

`body { font-family: var(--sans); }` already reads the token, so only `index.html` needs a literal:

```html
<meta name="theme-color" content="#f9f6f2" />
```

`theme.ts` repaints this from `--dawn` on every theme change, so the literal is only the pre-paint fallback.

- [ ] **Step 5: Build and check nothing dangles**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed. Then `grep -n 'var(--serif)\|var(--danger)' src/styles.css` still reports the sites Tasks 3–6 rewrite; that is expected until Task 6 lands.

---

### Task 3: Fraunces confined to its five roles

**Files:**
- Modify: `web/src/styles.css` — every `font-family: var(--serif)` site (lines 113, 404, 621, 710, 1077, 1111, 1158, 1254, 1259, 1518, 1579, 1763, 1783, 1864, 1923, 1980 pre-edit)

**Interfaces:**
- Consumes: `--display` and `--sans` from Task 2.
- Produces: exactly five display roles in the rendered DOM; Task 7 enumerates them from a live page.

- [ ] **Step 1: Set `--display` on the five roles only**

`.brand`, `.now-brand`, `.login h1` (wordmark); `.pane-title` (view titles); `.nowcard-title`; `.now-counter`; `.memory-title`.

- [ ] **Step 2: Move every other former-serif site to `--sans`**

`.today-clear`, `.chat-head h2`, `.chat-empty`, `.turn.pending`, `.prose`, `.prose :is(h1,h2,h3,h4)`, `.memory-blank`, `.dialog-title`, `.letter`. `.turn.pending` keeps `font-style: italic` — the bundled Atkinson italic covers it.

- [ ] **Step 3: Take the mockups' display weights and sizes**

```css
.brand { font-weight: 560; }
.pane-title { font-weight: 420; font-size: 1.625rem; }   /* mockup h1.view: 26px */
.set-pane .pane-title { font-weight: 560; font-size: 1.25rem; }  /* mockup .pane h2: 20px */
.memory-title { font-weight: 560; }
.nowcard-title { font-weight: 420; }
.now-brand { font-weight: 560; }
.now-counter { font-weight: 420; }
```

The counter's cells are sized in `em` (`.6em` / `.32em` for the colon), so the family swap moves no cell boundary and the tick animation is untouched. Do not change those widths.

- [ ] **Step 4: Verify**

Run: `cd web && grep -c 'var(--display)' src/styles.css`
Expected: 7 (five roles, with the wordmark spread over three selectors).
Run: `grep -n 'var(--serif)' src/styles.css` → no output.

---

### Task 4: The spine gradient, the sidebar, and the mobile tabs

**Files:**
- Modify: `web/src/styles.css` — `.spine::before`, `.sidebar-item`, `.tabs button`, and the mobile-tab block
- Create: `web/src/navicon.tsx`
- Modify: `web/src/app.tsx:136-186`

**Interfaces:**
- Consumes: `--spine`, `--sunk`, `--ink`, `--quiet`, `--faint`, `--sun-ink` from Task 2.
- Produces: `NavIcon`, exported from `web/src/navicon.tsx` with the signature
  `function NavIcon({ id }: { id: 'today' | 'tasks' | 'chat' | 'memory' | 'settings' }): JSX.Element`
  — a decorative `<svg class="nav-icon" aria-hidden="true">`, used by both the sidebar and the tab bar in `app.tsx`.

- [ ] **Step 1: Make the spine a gradient**

```css
.spine::before {
  /* the day's arc, gold at the top through to night; decoration under the rows,
     which each carry their own time, dot and tag */
  background: var(--spine);
}
```

No other rule may read `--spine`: nothing in the timeline may be legible only as a colour on this 2 px rule. `.ev-time` (the wall clock), `.ev-dot` (state), `.ev-tag` (flexibility) and the Now line's own label all stay exactly as shipped.

- [ ] **Step 2: Write `web/src/navicon.tsx`**

Paths transcribed verbatim from the mockups' `<nav>` blocks.

```tsx
const PATHS: Record<string, string[]> = {
  today: [
    'M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1',
  ],
  tasks: ['M9 6h11M9 12h11M9 18h11', 'M4 6.5l1 1L7 5', 'M4 12.5l1 1L7 11'],
  chat: ['M21 12a8 8 0 0 1-8 8H5l-2 2V12a8 8 0 0 1 8-8h2a8 8 0 0 1 8 8z'],
  memory: [
    'M12 3a7 7 0 0 1 7 7c0 2-1 3.5-2 4.5S15.5 17 15.5 19h-7c0-2-.5-3.5-1.5-4.5S5 12 5 10a7 7 0 0 1 7-7z',
    'M9.5 22h5',
  ],
  settings: [
    'M19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.5-2.4 1a7 7 0 0 0-2-1.2L14 3h-4l-.5 2.6a7 7 0 0 0-2 1.2l-2.4-1-2 3.5 2 1.5A7 7 0 0 0 5 12c0 .4 0 .8.1 1.2l-2 1.5 2 3.5 2.4-1c.6.5 1.3.9 2 1.2L10 21h4l.5-2.6c.7-.3 1.4-.7 2-1.2l2.4 1 2-3.5-2-1.5c.1-.4.1-.8.1-1.2z',
  ],
}

export function NavIcon({ id }: { id: keyof typeof PATHS }) {
  return (
    <svg className="nav-icon" viewBox="0 0 24 24" aria-hidden="true">
      {id === 'today' && <circle cx="12" cy="12" r="4" />}
      {id === 'settings' && <circle cx="12" cy="12" r="3" />}
      {PATHS[id].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  )
}
```

- [ ] **Step 3: Mount the icons and swap the wordmark glyph in `app.tsx`**

The nav labels do not change: the mockups say "Talk" where the app says "Chat", and copy strings are frozen this step.

```tsx
<button key={t.id} className="sidebar-item" aria-current={tab === t.id} onClick={() => setTab(t.id)}>
  <NavIcon id={t.id} />
  {t.label}
</button>
```

The tab bar takes the same child pair; the CSS stacks it. `brand-glyph` keeps its class name and becomes a disc in CSS — no markup change beyond the icons.

- [ ] **Step 4: Restyle the sidebar and tabs**

```css
.nav-icon {
  width: 17px;
  height: 17px;
  flex: none;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.sidebar-item {
  display: flex;
  align-items: center;
  gap: 0.7rem;
  color: var(--quiet);
  font-weight: 400;
}
.sidebar-item[aria-current='true'] {
  background: var(--sunk);
  color: var(--ink);
  font-weight: 700;
}
.sidebar-item[aria-current='true'] .nav-icon { stroke: var(--sun-ink); }

.tabs button {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 3px;
  color: var(--faint);
  font-size: 0.66rem;
}
.tabs .nav-icon { width: 20px; height: 20px; }
.tabs button[aria-current='true'] { color: var(--sun-ink); font-weight: 700; }
```

`.brand-glyph` becomes the mocked disc:

```css
.brand-glyph {
  width: 14px;
  height: 14px;
  flex: none;
  transform: none;
  border-radius: 50%;
  background: var(--sun);
  box-shadow: 0 0 0 3px color-mix(in oklch, var(--sun) 25%, var(--dawn));
}
```

- [ ] **Step 5: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed. Tabs keep their ≥ 2.5 rem height from the existing touch-target block; confirm the taller stacked tab does not push the `.shell` bottom padding — raise `calc(3.1rem + …)` to `calc(3.6rem + …)` and the toast's `calc(4.1rem + …)` to `calc(4.6rem + …)` to match.

---

### Task 5: Diamonds out

**Files:**
- Modify: `web/src/section.tsx`
- Modify: `web/src/views/Talk.tsx:478`
- Modify: `web/src/styles.css` — delete `.pane-glyph`, restyle `.chat-empty-glyph`, drop `.login h1::after`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `SectionTitle({ children, meta })` keeps its exact signature; only its first child element is removed.

- [ ] **Step 1: Delete the marker from `SectionTitle`**

```tsx
export function SectionTitle({ children, meta }: { children: string; meta?: string }) {
  return (
    <h2 className="pane-title">
      {children}
      {meta && <span className="pane-meta mono">{meta}</span>}
    </h2>
  )
}
```

- [ ] **Step 2: Delete `.pane-glyph` and `.login h1::after` from the sheet**

The heading rule keeps `display: flex` for the `meta` span; only the glyph goes.

- [ ] **Step 3: Turn the last two diamonds into discs**

The remaining rotated squares are decoration beside the wordmark and the Talk empty state, not section markers. The design language now has one mark — the sun disc — so they take it rather than staying the only diamonds in the app:

```css
.chat-empty-glyph {
  display: inline-block;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: var(--sun);
}
```

- [ ] **Step 4: Verify**

Run: `cd web && grep -rn 'rotate(45deg)' src/ | grep -v tick`
Expected: no output — the check mark's `rotate(45deg)` in `.tick::after` is the only one left, and it is a check, not a diamond.

---

### Task 6: The last literals — red out, faint back, sun off text

**Files:**
- Modify: `web/src/styles.css` — the `--danger` sites, the `--accent`-as-text sites, the tinted chips and rows, and the `--now-faint` block

**Interfaces:**
- Consumes: Task 2's tokens.
- Produces: a sheet in which no colour is a literal outside the two `:root` blocks, and `--danger` / `--now-faint` no longer exist.

- [ ] **Step 1: Retarget every error affordance to `--clay`**

`button.quiet.danger`, `button.ghost.danger:hover`, `.login form p[role='alert']`, `.chat-side-note.error`, `.turn.system` (border and text). Clay is already the app's failure colour — `.receipt.error .receipt-mark` has used it since step 8. Then delete `--danger` from both `:root` blocks.

- [ ] **Step 2: Take `--sun` off text**

`.tabs button[aria-current]` (done in Task 4), `.chat-new span`, `.chat-link`, `.prose a`, `.seg button[aria-pressed='true']` all become `var(--sun-ink)`.

- [ ] **Step 3: Give active rows the mockups' sunk treatment**

`.chat-row[data-active='true']`, `.chat-row[data-active='true'] .chat-open`, `.memory-row[aria-current='true']`, `.set-item[aria-current='true']` become `background: var(--sunk); color: var(--ink);` with `font-weight: 700` where the mockup bolds. `.set-item[aria-current]` keeps a `color-mix(in oklch, var(--sun) 30%, var(--mist))` border, as `.fchip.active` does.

- [ ] **Step 4: Make the category chips outline-only**

Any fill under `--sun-ink` or `--moss` drops them below 4.5:1 in light; the memory mockup draws them as outlines, which does not.

```css
.memory-cat {
  color: var(--cat, var(--quiet));
  border: 1px solid color-mix(in oklch, var(--cat, var(--mist)) 30%, var(--mist));
  background: none;
}
.chip[aria-pressed='true'] {
  background: var(--sunk);
  color: var(--ink);
  border-color: color-mix(in oklch, var(--cat, var(--sun)) 34%, var(--mist));
  font-weight: 700;
}
```

- [ ] **Step 5: Retire `--now-faint` and hand `--faint` back to the six collapsed sites**

Delete the `--now-faint` declaration from `.now-screen` and point `.now-top`, `.now-denom`, `.now-step`, `.now-next`, `.now-hint` at `var(--faint)`. Then restore the third grey where the step comments recorded its loss: `.ev-time`, `.ev-tag`, `.ev-range`, `.today-tomorrow`, `.task-add-hint`, `.task-group-why`. Update the two block comments in `styles.css` that describe the collapse — they no longer describe the sheet.

- [ ] **Step 6: Verify**

Run: `cd web && grep -n 'now-faint\|--danger' src/styles.css`
Expected: no output.
Run: `grep -nE '#[0-9a-fA-F]{3,8}|rgb\(|oklch\(' src/styles.css | grep -v ':root'`
Expected: only `oklch(0 0 0 / …)` shadows and scrims, which are neutral blacks at low alpha and identical in both themes.

---

### Task 7: Verification and commit

**Files:** none modified.

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Type-check and build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: no diagnostics; a `dist/` containing four `.woff2` assets.

- [ ] **Step 2: Confirm the Rust side is untouched**

Run: `cargo test --workspace`
Expected: 264 passing.

- [ ] **Step 3: Serve `dist/` and drive the system chromium over CDP**

Playwright's bundled Chromium cannot start on this host (no `libgbm.so.1`); use `/etc/profiles/per-user/shuntia/bin/chromium` with `--headless --remote-debugging-port` and speak CDP directly.

Capture `Network.requestWillBeSent` for the whole session and assert no request URL host is outside `127.0.0.1`. Then assert the faces really loaded:

```js
document.fonts.check('16px "Atkinson Hyperlegible"')
document.fonts.check('420 82px "Fraunces"')
```

Expected: both `true`, and zero requests to `fonts.googleapis.com`, `fonts.gstatic.com`, or any other external host.

- [ ] **Step 4: Walk both themes across all five views plus the Now screen**

For each, set `data-theme` to `light` then `dark`, and read back the computed colour of every element. Assert no rendered colour is a literal that exists in only one theme — every colour must trace to a token that both blocks define.

- [ ] **Step 5: Enumerate Fraunces across the rendered DOM**

Walk every element of every view, read `getComputedStyle(el).fontFamily`, and collect those whose first family is `Fraunces`. Assert the set is exactly the five roles, keyed by class: `brand` / `now-brand` / `login h1`, `pane-title`, `nowcard-title`, `now-counter`, `memory-title`.

- [ ] **Step 6: Re-run the counter's invariance checks**

Measure each `.cell` width across a full minute of ticks and assert it never changes; assert an unchanged cell's DOM node is never replaced (the tick spec's no-mutation rule). The `em` sizing means the font swap cannot move these, but measure rather than assume.

- [ ] **Step 7: Commit**

```bash
git add web/src/styles.css web/src/app.tsx web/src/navicon.tsx web/src/section.tsx \
        web/src/views/Talk.tsx web/index.html web/src/fonts \
        docs/superpowers/plans/2026-09-01-daylight-step-10-tokens-type.md
git commit -m "feat: daylight tokens, self-hosted type, and theme pass"
```

`flake.lock` was staged before this work began and is not part of this change; never `git add -A`.

---

## Self-review

**Spec coverage.** Step 10's five items map to Tasks 2 (tokens), 1+2+3 (fonts), 4 (spine, sidebar, tabs), 5 (diamonds). The three acceptance boxes map to Task 7 steps 3, 4 and 5. The global constraints' contrast floor is Task 6 and the table above; "never introduce red" is Task 6 step 1; "no external requests" is Task 1 plus Task 7 step 3; `prefers-reduced-motion` is untouched — no animation is added or removed in this step, and the existing reduce blocks still cover every transition the sheet declares.

**Placeholders.** The only elision is Task 2 step 3's "identical declaration list", which is spelled out immediately above it in the same step and must be duplicated verbatim; CSS has no way to share one list between an attribute selector and a media query without a preprocessor.

**Type consistency.** `NavIcon`'s `id` is the same `Tab` union `app.tsx` already keys its `NAV` array on, so the map is total and `tsc` proves it. `SectionTitle` keeps its exact prop signature, so its five call sites need no edit.
