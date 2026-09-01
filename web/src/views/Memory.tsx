import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import { SectionTitle } from '../section'
import type { MemoryFact, MemoryHit } from '../types'

const CATEGORIES = ['semantic', 'episodic', 'procedural'] as const

// matches the stylesheet's master-detail breakpoint
const SINGLE_PANE = '(max-width: 1087.98px)'

const DAY_MS = 86_400_000

type Category = (typeof CATEGORIES)[number]
type Filter = 'all' | Category

const LABEL: Record<Category, string> = {
  semantic: 'About you',
  episodic: 'Moments',
  procedural: 'How you work',
}

function categoryLabel(category: string): string {
  return category in LABEL ? LABEL[category as Category] : category
}

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

// Day-boundary distance, not elapsed hours: 23:00 yesterday reads "yesterday".
function relativeDay(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  const midnight = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  const days = Math.round((midnight(new Date()) - midnight(at)) / DAY_MS)
  if (days <= 0) return 'today'
  if (days === 1) return 'yesterday'
  if (days < 7) return `${days} days ago`
  if (days < 14) return 'last week'
  return shortDate(iso)
}

function savedLabel(iso: string): string {
  const day = relativeDay(iso)
  return day === 'today' || day === 'yesterday' ? `saved ${day}` : day
}

function clockTime(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  return at.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
}

// Facts never change once written, so reads are cached for the session; the list
// endpoint carries summaries only, and both a row's date and the note a fact
// replaces have to be read back one fact at a time.
const factCache = new Map<string, MemoryFact>()
const inFlight = new Set<string>()

type Facts = { get: (id: string) => MemoryFact | undefined; want: (id: string) => void }

function useFacts(): Facts {
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
  return { get: (id) => factCache.get(id), want }
}

function CategoryChip({ category }: { category: string }) {
  return <span className={`memory-cat cat-${category}`}>{categoryLabel(category)}</span>
}

export function Memory({ notify, refresh, openTalk }: ViewProps) {
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('all')
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
      .memoryList(query ? { q: query } : filter === 'all' ? {} : { category: filter })
      .then((res) => {
        if (era === listEra.current) setItems(res.items)
      })
      .catch(() => {
        if (era !== listEra.current) return
        setListFailed(true)
        notify("Couldn't load memories. Try again.")
      })
  }, [query, filter, notify])

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

  const searching = query !== ''
  const count = items?.length ?? 0
  const scope = searching ? 'found' : 'saved'

  return (
    <div className="page memory" data-pane={selected === null ? 'list' : 'detail'}>
      <div className="memory-side">
        <SectionTitle meta={count === 0 || listFailed ? undefined : `${count} ${scope}`}>
          Memory
        </SectionTitle>
        <p className="memory-lede">
          What Note has learned as you talk. Nothing here is ever deleted — replaced notes move to
          the archive.
        </p>
        <div className="memory-search">
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="7" cy="7" r="4.5" />
            <line x1="10.5" y1="10.5" x2="14" y2="14" />
          </svg>
          <input
            type="search"
            value={draft}
            placeholder="Search memories…"
            aria-label="Search memories"
            onChange={(e) => setDraft(e.target.value)}
          />
        </div>
        {!searching && (
          <div className="memory-chips" role="group" aria-label="Filter by category">
            <button className="chip" aria-pressed={filter === 'all'} onClick={() => setFilter('all')}>
              All
            </button>
            {CATEGORIES.map((c) => (
              <button
                key={c}
                className={`chip cat-${c}`}
                aria-pressed={filter === c}
                onClick={() => setFilter(c)}
              >
                {LABEL[c]}
              </button>
            ))}
          </div>
        )}
        {listFailed ? (
          <p className="muted memory-empty">
            Couldn't load memories.{' '}
            <button className="quiet" onClick={load}>
              Retry
            </button>
          </p>
        ) : items === null ? null : count === 0 ? (
          <p className="muted memory-empty">
            {searching || filter !== 'all'
              ? 'Nothing matches.'
              : 'No memories yet — the assistant saves what it learns as you talk.'}
          </p>
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
          <button className="ghost memory-back" onClick={() => setSelected(null)}>
            <span aria-hidden="true">←</span> Memories
          </button>
        )}
        {selected === null ? (
          count > 0 && <p className="memory-blank">Pick a memory to read it.</p>
        ) : fact === null ? (
          <p className="muted">Loading…</p>
        ) : (
          <FactBody
            fact={fact}
            facts={facts}
            onOpen={openFact}
            onAsk={(f) => openTalk(`About the memory "${f.summary}" — `)}
          />
        )}
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
        <span className="memory-rowmeta">
          <CategoryChip category={hit.category} />
          {saved && <span>{savedLabel(saved)}</span>}
        </span>
      </button>
    </li>
  )
}

function FactBody({
  fact,
  facts,
  onOpen,
  onAsk,
}: {
  fact: MemoryFact
  facts: Facts
  onOpen: (id: string) => void
  onAsk: (fact: MemoryFact) => void
}) {
  const previous = fact.supersedes
  const { want } = facts
  useEffect(() => {
    if (previous) want(previous)
  }, [previous, want])
  // The replaced note is named only once it has been read back, so the link
  // never offers to open something that cannot be resolved.
  const older = previous === null ? undefined : facts.get(previous)
  const time = clockTime(fact.created)

  return (
    <article>
      <p className="memory-meta">
        <CategoryChip category={fact.category} />
        {fact.archived && <span className="memory-flag">Archived</span>}
      </p>
      <h2 className="memory-title">{fact.summary}</h2>
      <Markdown text={fact.body} />
      <div className="memory-dmeta">
        <span>
          Saved from Talk · {relativeDay(fact.created)}
          {time && `, ${time}`}
        </span>
        {older && (
          <span>
            Replaces a note from {shortDate(older.created)} ·{' '}
            <button className="memory-prev" onClick={() => onOpen(older.id)}>
              see what changed
            </button>
          </span>
        )}
      </div>
      <button className="memory-ask" onClick={() => onAsk(fact)}>
        Ask Note about this
      </button>
    </article>
  )
}
