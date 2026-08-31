import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import { SectionTitle } from '../section'
import type { MemoryFact, MemoryHit } from '../types'

const CATEGORIES = ['semantic', 'episodic', 'procedural'] as const

// matches the stylesheet's master-detail breakpoint
const SINGLE_PANE = '(max-width: 1087.98px)'

type Category = (typeof CATEGORIES)[number]
type Filter = 'all' | Category

const LABEL: Record<Category, string> = {
  semantic: 'Semantic',
  episodic: 'Episodic',
  procedural: 'Procedural',
}

function categoryLabel(category: string): string {
  return category in LABEL ? LABEL[category as Category] : category
}

function shortDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  return at.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function CategoryChip({ category }: { category: string }) {
  return <span className={`memory-cat cat-${category}`}>{categoryLabel(category)}</span>
}

export function Memory({ notify, refresh }: ViewProps) {
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('all')
  const [items, setItems] = useState<MemoryHit[] | null>(null)
  const [listFailed, setListFailed] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)
  const [fact, setFact] = useState<MemoryFact | null>(null)

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
  const scope = searching ? 'found' : filter === 'all' ? 'saved' : filter

  return (
    <div className="page memory" data-pane={selected === null ? 'list' : 'detail'}>
      <div className="memory-side">
        <SectionTitle meta={count === 0 || listFailed ? undefined : `${count} ${scope}`}>Memory</SectionTitle>
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
              <li key={m.id}>
                <button
                  className="memory-row"
                  aria-current={selected === m.id}
                  onClick={() => openFact(m.id)}
                >
                  <span className="memory-summary">{m.summary}</span>
                  <CategoryChip category={m.category} />
                </button>
              </li>
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
          <FactBody fact={fact} onOpen={openFact} />
        )}
      </div>
    </div>
  )
}

function FactBody({ fact, onOpen }: { fact: MemoryFact; onOpen: (id: string) => void }) {
  const previous = fact.supersedes
  return (
    <article>
      <h2 className="memory-title">{fact.summary}</h2>
      <p className="memory-meta">
        <CategoryChip category={fact.category} />
        <span className="mono">{shortDate(fact.created)}</span>
        {fact.archived && <span className="memory-flag">Archived</span>}
        {previous && (
          <button className="memory-prev" onClick={() => onOpen(previous)}>
            Earlier version
          </button>
        )}
      </p>
      <Markdown text={fact.body} />
    </article>
  )
}
