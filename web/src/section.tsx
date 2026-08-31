export function SectionTitle({ children, meta }: { children: string; meta?: string }) {
  return (
    <h2 className="pane-title">
      <span className="pane-glyph" aria-hidden="true" />
      {children}
      {meta && <span className="pane-meta mono">{meta}</span>}
    </h2>
  )
}
