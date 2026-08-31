# llama.cpp webui ("llama-ui") design reference

Source of truth: `scratchpad/llamacpp/tools/ui` (SvelteKit + Tailwind v4 + shadcn-svelte),
crawled live at 1280×900 and 420×860, light + dark. Screenshots in `scratchpad/llamaui-shots/`.

## Layout anatomy (desktop 1280px)

- **Far-left icon rail**, ~56px wide, full height, `--sidebar` surface: logo at top, then
  icon buttons — new chat (pencil), search, settings (gear). No text labels. The rail is
  the only permanent chrome on the left; a full conversation sidebar expands from it.
- **Conversation tabs across the top** of the content area: pill-shaped chips with a
  truncated title + close ×, and a `+` button after the last tab. Tabs sit directly on the
  background (no toolbar bar behind them).
- **Chat body fills the whole remaining width**, but the *message column* is centered at
  `max-w-[48rem]` (768px). Full-bleed background, constrained content.
- **User messages**: right-aligned pill bubbles (`--secondary`/muted bg, rounded ~1.5rem,
  `max-w-[80%]`). **Assistant messages**: plain text on the background, no bubble,
  left-aligned, full column width.
- **Hover action rows** under each message: small ghost icon buttons (copy, edit,
  regenerate, branch, delete) — invisible until hover, no borders.
- **Composer**: large detached card centered at the bottom of the column,
  `rounded-4xl md:rounded-3xl`, `shadow-sm` → `shadow-md` on focus-within, backdrop-blur.
  Row 1: borderless textarea, placeholder "Type a message...". Row 2: `+` attach button
  (circular ghost) on the left; model-selector chip (icon + name + chevron, pill) and
  circular filled send button (arrow-up) on the right.
- **Modals** (settings, errors): centered, `rounded-xl`, `--popover` surface, 1px
  `--border`, title row = icon + bold title, × close top-right. Settings dialog is a large
  fixed-size panel (~1150×830 at 1280) with a **left vertical tab list** (icon + label,
  selected = `--accent` filled rounded-lg) and a scrollable right pane; footer row has
  "Reset to default" (secondary) left, "Save settings" (primary) right.
- **Settings field pattern**: label (sm, medium) → control → helper text
  (`--muted-foreground`, sm) below; checkboxes as filled dark squares with white check;
  generous vertical rhythm (~2rem between fields).

## Palette (verbatim from `src/app.css`; Tailwind v4 `@theme inline` tokens)

Neutral monochrome (oklch chroma 0) everywhere; color only for destructive/charts.

| Token | Light | Dark (`.dark`) |
|---|---|---|
| --radius | 0.625rem | — |
| --background | oklch(1 0 0) | oklch(0.16 0 0) |
| --foreground | oklch(0.145 0 0) | oklch(0.985 0 0) |
| --card | oklch(1 0 0) | oklch(0.205 0 0) |
| --popover | oklch(1 0 0) | oklch(0.205 0 0) |
| --primary | oklch(0.205 0 0) | oklch(0.922 0 0) |
| --primary-foreground | oklch(0.985 0 0) | oklch(0.205 0 0) |
| --secondary | oklch(0.95 0 0) | oklch(0.29 0 0) |
| --muted | oklch(0.97 0 0) | oklch(0.269 0 0) |
| --muted-foreground | oklch(0.556 0 0) | oklch(0.708 0 0) |
| --accent (hover/selected fill) | oklch(0.95 0 0) | oklch(0.269 0 0) |
| --destructive | oklch(0.577 0.245 27.325) | oklch(0.704 0.191 22.216) |
| --border | oklch(0.875 0 0) | oklch(1 0 0 / 30%) |
| --input | oklch(0.92 0 0) | oklch(1 0 0 / 30%) |
| --ring | oklch(0.708 0 0) | oklch(0.556 0 0) |
| --sidebar | oklch(0.985 0 0) | oklch(0.2 0 0) |
| --sidebar-border | oklch(0.922 0 0) | oklch(1 0 0 / 10%) |
| --code-background | oklch(0.985 0 0) | oklch(0.225 0 0) |

Key moves: dark mode is *soft* dark (bg 0.16, cards 0.205 — not pure black); dark borders
are translucent white (30% for controls, 10% for sidebar); primary button inverts
(near-black on light, near-white on dark).

## Typography & density

- System sans stack (Tailwind default); mono stack pinned via `--font-mono`
  (ui-monospace, SF Mono, Cascadia Code, …) for `code/pre/kbd`.
- Body ~15–16px; helper/meta text sm (~13–14px) in `--muted-foreground`; dialog titles
  ~20px bold with a leading icon.
- Airy: settings fields ~2rem apart, composer is tall (min-h-12 + padding), message column
  has large top padding (`--chat-form-padding-top: 6rem`).

## Radii, shadows, motion

- Radius scale from `--radius: 0.625rem` (sm −4px, md −2px, lg =, xl +4px); big surfaces
  go far beyond: composer rounded-3xl/4xl, bubbles ~1.5rem, tabs full pill.
- Shadows minimal: `shadow-sm` resting, `shadow-md` on focus; dialogs get a soft large
  shadow. Elevation via surface tone + border more than shadow.
- Motion restrained: `transition-all` on interactive bits, a `shimmer-text` gradient
  animation for "thinking" text (reduced-motion aware). Scrollbars 6px, transparent until
  hover.

## Screenshot index (`llamaui-shots/`)

- 01/02 desktop light/dark empty main; 03/04 mobile 420 light/dark
- 05 (blank — render race, ignore), 06 early conversation shot
- 07 settings first open; 08-settings-{light,dark}-{general,display,tools,agentic,import-export,sampling-penalties,developer}
- 09 dark composer typed; 10/11 dark/light conversation (user bubble, hover actions,
  error modal, composer with model chip)

## Translating to Note

1. **Adopt the token system wholesale, keep Note's identity in the accent.** Map Note's
   existing CSS variables to the llama-ui roles above (background/card/border/muted/
   primary…), neutralize the current tinted surfaces toward the monochrome scale, but keep
   `--sun` as the single accent for primary actions, active nav, focus ring, and the
   selection states where llama-ui uses near-black `--primary`. Dark theme follows the
   soft-dark numbers (bg ≈ oklch 0.16, raised ≈ 0.205, translucent white borders).
2. **App shell**: replace the top tab bar with a llama-style left sidebar on desktop —
   icon + label nav (Today, Tasks, Chat, Memory, Settings) on a `--sidebar` surface with
   `--sidebar-border`; collapse to the existing bottom tab bar on mobile (<768px). Content
   area takes the full remaining width.
3. **Full width, constrained columns**: views span the viewport; reading columns cap at
   48rem (chat) / 64rem (Today, Tasks, Memory, Settings can go wider with multi-column
   layouts). No more single narrow centered card for everything.
4. **Chat**: user turns become right-aligned muted bubbles (max-80%); assistant turns
   plain on background; tool calls stay expandable blocks styled like llama-ui's cards
   (rounded-lg, `--card`, 1px border); composer becomes the detached rounded-3xl card with
   send as a circular filled button. Keep Note's conversation sidebar (llama-ui hides
   conversations behind search; ours is better UX) but restyle rows to the sidebar tokens.
5. **Settings**: master-detail like the settings dialog — left vertical section list
   (Profile, Schedule, Appearance, Persona, Notifications, Admin), right scrollable pane,
   llama-ui field pattern (label / control / helper text), sticky footer with Reset +
   Save.
6. **Controls**: checkboxes/selects/inputs per llama-ui — 1px `--input` borders,
   rounded-md, focus ring `--ring`; primary buttons filled (sun accent), secondary
   `--secondary`, ghost icon buttons for row actions that appear on hover.
7. **Modals & helper text**: every destructive/confirm flow uses the centered rounded-xl
   dialog pattern; every settings field gets a one-line muted helper sentence.
