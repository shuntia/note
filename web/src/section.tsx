export function SectionTitle({ children, meta }: { children: string; meta?: string }) {
  return (
    <h2 className="pane-title">
      {children}
      {meta && <span className="pane-meta mono">{meta}</span>}
    </h2>
  )
}
