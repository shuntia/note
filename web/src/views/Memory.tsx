import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from 'react'
import { api } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import { flip, popOut, rise, settle } from '../motion-gsap'
import { Overflow } from '../overflow'
import '../styles/memory.css'
import { InboxBar, InboxCard, InboxRows, useInbox } from './Inbox'
import type { MemoryFact, MemoryHit } from '../types'

// matches the stylesheet's master-detail breakpoint
const SINGLE_PANE = '(max-width: 1087.98px)'
const AUTO_OPEN_AFTER = 8

type Filter = '' | 'semantic' | 'episodic' | 'inbox'

const FILTERS: { id: Filter; label: string }[] = [
  { id: '', label: 'All' },
  { id: 'semantic', label: 'Facts' },
  { id: 'episodic', label: 'Episodes' },
  { id: 'inbox', label: 'Inbox' },
]

// An episode is written as "<date> · <thread title>: <what happened>"; the date has a
// column of its own and the thread title is not what the row is about.
function displaySummary(hit: MemoryHit): string {
  if (hit.category !== 'episodic') return hit.summary
  const text = hit.summary.replace(/^\d{4}-\d{2}-\d{2} · /, '')
  const at = text.indexOf(': ')
  return at >= 0 && at < 80 ? text.slice(at + 2) : text
}

// A fact's body may open with `key: value` lines; they are written for the model and
// read as a row of quiet chips rather than as prose.
function frontMatter(body: string): { meta: [string, string][]; rest: string } {
  const lines = body.split('\n')
  const meta: [string, string][] = []
  let i = 0
  for (; i < lines.length; i++) {
    const line = lines[i]
    if (line.trim() === '') {
      i += 1
      break
    }
    const at = line.indexOf(': ')
    if (at <= 0) break
    meta.push([line.slice(0, at).trim(), line.slice(at + 2).trim()])
  }
  if (meta.length === 0) return { meta, rest: body }
  return { meta, rest: lines.slice(i).join('\n') }
}

const unwrap = (value: string) =>
  value
    .replace(/^\[(.*)\]$/s, '$1')
    .replace(/^"(.*)"$/s, '$1')
    .trim()

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

// The list endpoint has no dates, so the order settles as the rows read theirs back;
// a row still waiting keeps the server's place until then.
function newestFirst(items: MemoryHit[], saved: (id: string) => MemoryFact | undefined): MemoryHit[] {
  return items
    .map((m, i) => ({ m, i, created: saved(m.id)?.created }))
    .sort((a, b) => {
      if (a.created && b.created) return b.created.localeCompare(a.created) || a.i - b.i
      if (a.created) return -1
      if (b.created) return 1
      return a.i - b.i
    })
    .map((row) => row.m)
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

// Results arrive as a staggered rise; a row that only changed places slides there.
function useListMotion(list: RefObject<HTMLUListElement | null>, sig: string) {
  const tops = useRef(new Map<string, number>())
  const settled = useRef(false)

  useLayoutEffect(() => {
    settled.current = true
    settle([...(list.current?.children ?? [])])
  }, [sig, list])

  useLayoutEffect(() => {
    const moves: { el: Element; dy: number }[] = []
    list.current?.querySelectorAll<HTMLElement>('[data-mem]').forEach((el) => {
      const id = el.dataset.mem as string
      const top = el.getBoundingClientRect().top
      const was = tops.current.get(id)
      if (was !== undefined) moves.push({ el, dy: was - top })
      tops.current.set(id, top)
    })
    if (settled.current) settled.current = false
    else flip(moves)
  })
}

export function Memory({ notify, refresh, openTalk }: ViewProps) {
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('')
  const [items, setItems] = useState<MemoryHit[] | null>(null)
  const [listFailed, setListFailed] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)
  const [fact, setFact] = useState<MemoryFact | null>(null)

  const facts = useFacts()
  const inbox = useInbox(refresh, notify)
  const onInbox = filter === 'inbox'
  const { page: inboxPage, selected: inboxSelected, item: inboxItem, open: openItem, close: closeItem } = inbox
  const detail = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLUListElement>(null)
  const card = useRef<HTMLElement>(null)
  useListMotion(list, items?.map((m) => m.id).join() ?? '')
  // request eras: a slow reply must never overwrite the results of a later one
  const listEra = useRef(0)
  const factEra = useRef(0)

  useEffect(() => {
    const id = window.setTimeout(() => setQuery(draft.trim()), 300)
    return () => window.clearTimeout(id)
  }, [draft])

  const load = useCallback(() => {
    if (filter === 'inbox') return
    const era = ++listEra.current
    setListFailed(false)
    api
      .memoryList(query ? { q: query } : filter ? { category: filter } : {})
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

  // Side by side, the reading pane is never left empty: it opens the newest fact
  // once the rows on screen have read back their dates.
  const head = items?.slice(0, AUTO_OPEN_AFTER) ?? []
  const dated = head.length > 0 && head.every((m) => facts.get(m.id) !== undefined)
  useEffect(() => {
    if (filter === 'inbox' || selected !== null || !items || !dated) return
    if (window.matchMedia(SINGLE_PANE).matches) return
    openFact(newestFirst(items, (id) => factCache.get(id))[0].id)
  }, [filter, items, dated, selected, openFact])

  useEffect(() => {
    if (!onInbox || inboxSelected !== null || !inboxPage?.items.length) return
    if (window.matchMedia(SINGLE_PANE).matches) return
    openItem(inboxPage.items[0].id)
  }, [onInbox, inboxSelected, inboxPage, openItem])

  const shown = onInbox ? inboxSelected : selected

  // On the single-pane layout the detail replaces the list, so bring it into view.
  useEffect(() => {
    if (shown === null) return
    if (!window.matchMedia(SINGLE_PANE).matches) return
    detail.current?.scrollIntoView()
  }, [shown])

  // Where the detail replaces the list it rises like a sheet; beside it, it settles.
  useLayoutEffect(() => {
    if (fact === null && inboxItem === null) return
    rise(card.current, window.matchMedia(SINGLE_PANE).matches ? 28 : 14)
  }, [fact, inboxItem])

  const closeDetail = () =>
    popOut(card.current, () => (onInbox ? closeItem() : setSelected(null)), true)

  const openMemory = (id: string) => {
    setFilter('')
    closeItem()
    openFact(id)
  }

  const filters = inboxPage?.items.length ? FILTERS : FILTERS.filter((f) => f.id !== 'inbox')
  const count = items?.length ?? 0

  return (
    <div className="memory" data-pane={shown === null ? 'list' : 'detail'}>
      <div className="memory-side">
        {!onInbox && (
          <div className="tellnote memory-search">
            <input
              type="search"
              value={draft}
              placeholder="Search what Note knows"
              aria-label="Search what Note knows"
              onChange={(e) => setDraft(e.target.value)}
            />
          </div>
        )}
        {(onInbox || draft.trim() === '') && (
          <div className="memory-filter">
            <div className="seg" role="group" aria-label="Show">
              {filters.map((f) => (
                <button
                  key={f.id || 'all'}
                  type="button"
                  aria-pressed={filter === f.id}
                  onClick={() => {
                    setFilter(f.id)
                    setSelected(null)
                    setFact(null)
                    closeItem()
                  }}
                >
                  {f.label}
                </button>
              ))}
            </div>
          </div>
        )}
        {onInbox ? (
          inboxPage && (
            <>
              {inboxPage.refresh && (
                <InboxBar
                  latest={inboxPage.latest}
                  refreshing={inbox.refreshing}
                  upToDate={inbox.upToDate}
                  onRefresh={inbox.pull}
                />
              )}
              <InboxRows rows={inboxPage.items} selected={inboxSelected} onOpen={openItem} />
              {inbox.more && (
                <button className="memory-link inbox-more" onClick={inbox.loadMore}>
                  More
                </button>
              )}
            </>
          )
        ) : listFailed ? (
          <p className="memory-empty">
            Couldn't load memories.{' '}
            <button className="memory-link" onClick={load}>
              Retry
            </button>
          </p>
        ) : items === null ? null : count === 0 ? (
          <p className="memory-empty">{query ? 'Nothing matches.' : 'Nothing yet.'}</p>
        ) : (
          <ul className="memory-list" ref={list}>
            {newestFirst(items, facts.get).map((m) => (
              <MemoryRow
                key={m.id}
                hit={m}
                saved={facts.get(m.id)?.created}
                selected={selected === m.id}
                onOpen={openFact}
                onSeen={facts.want}
                openTalk={openTalk}
              />
            ))}
          </ul>
        )}
      </div>
      <div className="memory-detail" ref={detail}>
        {shown !== null && (
          <button className="memory-back" onClick={closeDetail}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M15 6l-6 6 6 6" />
            </svg>
            Memory
          </button>
        )}
        {onInbox
          ? inboxItem !== null && (
              <InboxCard item={inboxItem} cardRef={card} onMemory={openMemory} />
            )
          : selected !== null &&
            fact !== null && <FactBody fact={fact} openTalk={openTalk} cardRef={card} />}
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
  openTalk,
}: {
  hit: MemoryHit
  saved: string | undefined
  selected: boolean
  onOpen: (id: string) => void
  onSeen: (id: string) => void
  openTalk: (draft: string) => void
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
    <li ref={row} data-mem={hit.id}>
      <button className="memory-row" aria-current={selected} onClick={() => onOpen(hit.id)}>
        <Glyph category={hit.category} />
        <span className="memory-summary">{displaySummary(hit)}</span>
        {saved && <span className="memory-when">{factDate(saved)}</span>}
      </button>
      <Overflow
        className="memory-more-wrap"
        row="li[data-mem]"
        trigger={false}
        label={`More actions for ${displaySummary(hit)}`}
        items={[
          { label: 'Open', run: () => onOpen(hit.id) },
          { label: 'Tell Note more', run: () => openTalk(`About "${hit.summary}": `) },
          { label: "That's wrong", run: () => openTalk(`This is wrong: "${hit.summary}". `) },
        ]}
      />
    </li>
  )
}

function Glyph({ category }: { category: string }) {
  if (category !== 'episodic') return <span className="memory-dot" aria-hidden="true" />
  return (
    <svg className="memory-clock" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8" />
      <path d="M12 7.5v4.75l3 1.75" />
    </svg>
  )
}

function FactBody({
  fact,
  openTalk,
  cardRef,
}: {
  fact: MemoryFact
  openTalk: (draft: string) => void
  cardRef: RefObject<HTMLElement | null>
}) {
  const { meta, rest } = frontMatter(fact.body)
  return (
    <article className="memory-card" ref={cardRef}>
      <h2 className="memory-title">{displaySummary(fact)}</h2>
      <p className="memory-meta">
        From a chat on {shortDate(fact.created)}
        {fact.archived && <span className="memory-flag">archived</span>}
      </p>
      {meta.length > 0 && (
        <div className="memory-facts">
          {meta.flatMap(([key, value]) =>
            key === 'tags'
              ? unwrap(value)
                  .split(',')
                  .map((tag) => tag.trim())
                  .filter(Boolean)
                  .map((tag) => (
                    <span className="meta" key={`tag:${tag}`}>
                      {tag}
                    </span>
                  ))
              : [
                  <span className="meta" key={key}>
                    {key} {unwrap(value)}
                  </span>,
                ],
          )}
        </div>
      )}
      {rest.trim() !== '' && rest.trim() !== fact.summary.trim() && <Markdown text={rest} />}
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
