type NavId = 'today' | 'tasks' | 'chat' | 'memory' | 'settings'

const PATHS: Record<NavId, string[]> = {
  today: ['M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1'],
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

// The button around it carries the name, so the glyph is decoration.
export function NavIcon({ id }: { id: NavId }) {
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
