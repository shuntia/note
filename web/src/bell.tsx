const BODY = 'M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9'
const CLAPPER = 'M13.7 21a2 2 0 0 1-3.4 0'

// Struck when the routine is silent. Without a `label` the glyph is decoration —
// the control around it is carrying the name.
export function Bell({ on, label }: { on: boolean; label?: string }) {
  return (
    <svg
      className="bell"
      viewBox="0 0 24 24"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <path d={BODY} />
      <path d={CLAPPER} />
      {!on && <path d="M3 3l18 18" />}
    </svg>
  )
}
