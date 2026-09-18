import { useState } from 'react'
import type { HistoryRow } from './types'

const FOLD_KEY = 'note.sofarFolded'

function readFold(): boolean {
  try {
    return localStorage.getItem(FOLD_KEY) !== 'false'
  } catch {
    return true
  }
}

function writeFold(folded: boolean) {
  try {
    localStorage.setItem(FOLD_KEY, String(folded))
  } catch {
    // storage blocked; the fold still holds for this session
  }
}

function Check() {
  return (
    <svg className="glyph" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5 12.5l4.5 4.5L19 7.5" />
    </svg>
  )
}

function Arrow() {
  return (
    <svg className="glyph" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4 12h15M13 6l6 6-6 6" />
    </svg>
  )
}

function mark(kind: HistoryRow['kind']) {
  if (kind === 'event_done' || kind === 'task_done' || kind === 'checkin') return <Check />
  if (kind === 'event_moved') return <Arrow />
  return <span className="sofar-dot" aria-hidden="true" />
}

// What the day has already settled, folded away until asked for.
export function SoFar({ rows }: { rows: HistoryRow[] }) {
  const [folded, setFolded] = useState(readFold)

  const toggle = () => {
    const next = !folded
    setFolded(next)
    writeFold(next)
  }

  return (
    <section className={`sofar${folded ? '' : ' open'}`}>
      <button className="sofar-fold" aria-expanded={!folded} onClick={toggle}>
        <span className="sofar-lead">So far today</span>
        {rows.length > 0 && <span className="sofar-count tnum">{rows.length}</span>}
        <span className="sofar-chev" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </span>
      </button>
      {!folded &&
        (rows.length === 0 ? (
          <p className="sofar-empty">Nothing yet</p>
        ) : (
          <ul className="sofar-list">
            {rows.map((row, i) => (
              <li key={`${row.at}-${i}`} className={row.kind.replace('_', '-')}>
                <span className="sofar-when tnum">{row.time}</span>
                <span className="sofar-mark">{mark(row.kind)}</span>
                <span className="sofar-what">{row.label}</span>
              </li>
            ))}
          </ul>
        ))}
    </section>
  )
}
