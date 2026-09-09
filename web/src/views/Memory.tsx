import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import type { MemoryFact, MemoryHit } from '../types'

// matches the stylesheet's master-detail breakpoint
const SINGLE_PANE = '(max-width: 1087.98px)'

function shortDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  const sameYear = at.getFullYear() === new Date().getFullYear()
  return at.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    ...(sameYear ? {} : { year: 'numeric' }),
  })
}

// Day boundaries, not elapsed hours: something saved at 00:30 still reads "today".
function factDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  const midnight = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  return midnight(at) === midnight(new Date()) ? 'today' : shortDate(iso)
}

// Facts never change once written, so reads are cached for the session; the list
// endpoint carries summaries only, so a row's date has to be read back one fact
// at a time.
const factCache = new Map<string, MemoryFact>()
const inFlight = new Set<string>()

function useFacts() {
  const [, bump] = useState(0)
  const want = useCallback((id: string) => {
    if (factCache.has(id) || inFlight.has(id)) return
    inFlight.add(id)
    api
      .memoryRead(id)
      .then((f) => {
        factCache.set(f.id, f)
        bump((n) => n + 1)
      })
      .catch(() => {
        // a date that will not load simply stays off the row
      })
      .finally(() => inFlight.delete(id))
  }, [])
  return { get: (id: string) => factCache.get(id), want }
}

export function Memory({ notify, refresh, openTalk }: ViewProps) {
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [items, setItems] = useState<MemoryHit[] | null>(null)
  const [listFailed, setListFailed] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)
  const [fact, setFact] = useState<MemoryFact | null>(null)

  const facts = useFacts()
  const detail = useRef<HTMLDivElement>(null)
  // request eras: a slow reply must never overwrite the results of a later one
  const listEra = useRef(0)
  const factEra = useRef(0)

  useEffect(() => {
    const id = window.setTimeout(() => setQuery(draft.trim()), 300)
    return () => window.clearTimeout(id)
  }, [draft])

  const load = useCallback(() => {
    const era = ++listEra.current
    setListFailed(false)
    api
      .memoryList(query ? { q: query } : {})
      .then((res) => {
        if (era === listEra.current) setItems(res.items)
      })
      .catch(() => {
        if (era !== listEra.current) return
        setListFailed(true)
        notify("Couldn't load memories. Try again.")
      })
  }, [query, notify])

  useEffect(load, [load, refresh])

  const openFact = useCallback(
    (id: string) => {
      setSelected(id)
      setFact(null)
      const era = ++factEra.current
      api
        .memoryRead(id)
        .then((f) => {
          factCache.set(f.id, f)
          if (era === factEra.current) setFact(f)
        })
        .catch(() => {
          if (era !== factEra.current) return
          notify("Couldn't open that memory. Try again.")
          setSelected(null)
        })
    },
    [notify],
  )

  // On the single-pane layout the detail replaces the list, so bring it into view.
  useEffect(() => {
    if (selected === null) return
    if (!window.matchMedia(SINGLE_PANE).matches) return
    detail.current?.scrollIntoView()
  }, [selected])

  const count = items?.length ?? 0

  return (
    <div className="memory" data-pane={selected === null ? 'list' : 'detail'}>
      <div className="memory-side">
        <div className="tellnote memory-search">
          <input
            type="search"
            value={draft}
            placeholder="Search what Note knows"
            aria-label="Search what Note knows"
            onChange={(e) => setDraft(e.target.value)}
          />
        </div>
        {listFailed ? (
          <p className="memory-empty">
            Couldn't load memories.{' '}
            <button className="memory-link" onClick={load}>
              Retry
            </button>
          </p>
        ) : items === null ? null : count === 0 ? (
          <p className="memory-empty">{query ? 'Nothing matches.' : 'Nothing yet.'}</p>
        ) : (
          <ul className="memory-list">
            {items.map((m) => (
              <MemoryRow
                key={m.id}
                hit={m}
                saved={facts.get(m.id)?.created}
                selected={selected === m.id}
                onOpen={openFact}
                onSeen={facts.want}
              />
            ))}
          </ul>
        )}
      </div>
      <div className="memory-detail" ref={detail}>
        {selected !== null && (
          <button className="memory-back" onClick={() => setSelected(null)}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M15 6l-6 6 6 6" />
            </svg>
            Memory
          </button>
        )}
        {selected !== null && fact !== null && <FactBody fact={fact} openTalk={openTalk} />}
      </div>
    </div>
  )
}

function MemoryRow({
  hit,
  saved,
  selected,
  onOpen,
  onSeen,
}: {
  hit: MemoryHit
  saved: string | undefined
  selected: boolean
  onOpen: (id: string) => void
  onSeen: (id: string) => void
}) {
  const row = useRef<HTMLLIElement>(null)

  // Dates cost one read each, so a row only asks for its own once it is on screen.
  useEffect(() => {
    if (saved !== undefined) return
    const el = row.current
    if (!el || typeof IntersectionObserver !== 'function') {
      onSeen(hit.id)
      return
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return
        io.disconnect()
        onSeen(hit.id)
      },
      { rootMargin: '200px' },
    )
    io.observe(el)
    return () => io.disconnect()
  }, [hit.id, saved, onSeen])

  return (
    <li ref={row}>
      <button className="memory-row" aria-current={selected} onClick={() => onOpen(hit.id)}>
        <span className="memory-summary">{hit.summary}</span>
        {saved && <span className="memory-when">{factDate(saved)}</span>}
      </button>
    </li>
  )
}

function FactBody({ fact, openTalk }: { fact: MemoryFact; openTalk: (draft: string) => void }) {
  return (
    <article className="memory-card">
      <h2 className="memory-title">{fact.summary}</h2>
      <p className="memory-meta">
        From a chat on {shortDate(fact.created)}
        {fact.archived && <span className="memory-flag">archived</span>}
      </p>
      {fact.body.trim() !== fact.summary.trim() && <Markdown text={fact.body} />}
      <div className="memory-acts">
        <button
          className="btn-haze"
          onClick={() => openTalk(`This is wrong: "${fact.summary}". `)}
        >
          That's wrong
        </button>
        <button className="memory-more" onClick={() => openTalk(`About "${fact.summary}": `)}>
          Tell Note more
        </button>
      </div>
    </article>
  )
}
