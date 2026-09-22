export function Tick({
  checked,
  label,
  onClick,
}: {
  checked: boolean
  label: string
  onClick: () => void
}) {
  return (
    <button className="tick" role="checkbox" aria-checked={checked} aria-label={label} onClick={onClick}>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M7 12.4l3.2 3.1 6.4-6.6" />
      </svg>
    </button>
  )
}
