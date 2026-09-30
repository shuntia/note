import { useCallback, useEffect, useRef, useState, type RefObject } from 'react'
import { api } from '../api'
import {
  arrivalLabel,
  mergePage,
  outcomeLabel,
  REFRESH_POLL_MS,
  REFRESH_WINDOW_MS,
  refreshSettled,
  UP_TO_DATE_MS,
} from '../inbox'
import type { InboxItem, InboxKind, InboxOutcome, InboxPage, InboxRow } from '../types'

const PAGE_SIZE = 50

const sleep = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms))

export function useInbox(refresh: number, notify: (msg: string) => void) {
  const [page, setPage] = useState<InboxPage | null>(null)
  const [more, setMore] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [upToDate, setUpToDate] = useState(false)
  const [selected, setSelected] = useState<number | null>(null)
  const [item, setItem] = useState<InboxItem | null>(null)
  const pageEra = useRef(0)
  const itemEra = useRef(0)
  const alive = useRef(true)

  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  const load = useCallback(async (): Promise<InboxPage> => {
    const era = ++pageEra.current
    const p = await api.inboxList({ limit: PAGE_SIZE })
    if (era === pageEra.current) {
      setPage(p)
      setMore(p.items.length === PAGE_SIZE)
    }
    return p
  }, [])

  useEffect(() => {
    load().catch(() => undefined)
  }, [load, refresh])

  const loadMore = useCallback(() => {
    if (!page || page.items.length === 0) return
    const last = page.items[page.items.length - 1]
    const era = pageEra.current
    api
      .inboxList({ before: last.received_at, limit: PAGE_SIZE })
      .then((p) => {
        if (era !== pageEra.current) return
        setPage((cur) => (cur ? { ...cur, items: mergePage(cur.items, p.items) } : cur))
        setMore(p.items.length === PAGE_SIZE)
      })
      .catch(() => notify("Couldn't load more. Try again."))
  }, [page, notify])

  const pull = useCallback(async () => {
    setRefreshing(true)
    setUpToDate(false)
    const started = Date.now()
    try {
      const { requested_at } = await api.inboxRefresh()
      let latest: string | null = null
      while (alive.current && !refreshSettled(requested_at, latest, Date.now() - started)) {
        await sleep(Math.min(REFRESH_POLL_MS, Math.max(0, REFRESH_WINDOW_MS - (Date.now() - started))))
        const p = await load().catch(() => null)
        if (p) latest = p.latest
      }
      if (alive.current) setUpToDate(true)
    } catch {
      notify("Couldn't refresh. Try again.")
    } finally {
      if (alive.current) setRefreshing(false)
    }
  }, [load, notify])

  useEffect(() => {
    if (!upToDate) return
    const id = window.setTimeout(() => setUpToDate(false), UP_TO_DATE_MS)
    return () => window.clearTimeout(id)
  }, [upToDate])

  const open = useCallback(
    (id: number) => {
      setSelected(id)
      setItem(null)
      const era = ++itemEra.current
      api
        .inboxRead(id)
        .then((it) => {
          if (era === itemEra.current) setItem(it)
        })
        .catch(() => {
          if (era !== itemEra.current) return
          notify("Couldn't open that item. Try again.")
          setSelected(null)
        })
    },
    [notify],
  )

  const close = useCallback(() => {
    ++itemEra.current
    setSelected(null)
    setItem(null)
  }, [])

  return { page, more, loadMore, refreshing, upToDate, pull, selected, item, open, close }
}

function Chips({ kind, outcome }: { kind: InboxKind; outcome: InboxOutcome | null }) {
  return (
    <span className="inbox-chips">
      <span className="inbox-chip">{kind}</span>
      <span className="inbox-chip" data-outcome={outcome ?? 'pending'}>
        {outcomeLabel(outcome)}
      </span>
    </span>
  )
}

export function InboxBar({
  latest,
  refreshing,
  upToDate,
  onRefresh,
}: {
  latest: string | null
  refreshing: boolean
  upToDate: boolean
  onRefresh: () => void
}) {
  return (
    <div className="inbox-bar">
      <button
        type="button"
        className="inbox-refresh"
        aria-label="Refresh"
        aria-busy={refreshing}
        disabled={refreshing}
        onClick={onRefresh}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M20 12a8 8 0 1 1-2.34-5.66" />
          <path d="M20 4v4h-4" />
        </svg>
      </button>
      <span className="inbox-when" aria-live="polite">
        {upToDate ? 'Up to date' : latest ? arrivalLabel(latest, new Date()) : ''}
      </span>
    </div>
  )
}

export function InboxRows({
  rows,
  selected,
  onOpen,
}: {
  rows: InboxRow[]
  selected: number | null
  onOpen: (id: number) => void
}) {
  const now = new Date()
  return (
    <ul className="memory-list">
      {rows.map((r) => (
        <li key={r.id}>
          <button className="memory-row" aria-current={selected === r.id} onClick={() => onOpen(r.id)}>
            <span className="inbox-text">
              <span className="memory-summary">{r.title}</span>
              <Chips kind={r.kind} outcome={r.outcome} />
            </span>
            <span className="memory-when">{arrivalLabel(r.received_at, now)}</span>
          </button>
        </li>
      ))}
    </ul>
  )
}

export function InboxCard({
  item,
  cardRef,
  onMemory,
}: {
  item: InboxItem
  cardRef: RefObject<HTMLElement | null>
  onMemory: (id: string) => void
}) {
  return (
    <article className="memory-card" ref={cardRef}>
      <h2 className="memory-title">{item.title}</h2>
      <p className="memory-meta">
        <Chips kind={item.kind} outcome={item.outcome} />
        <span className="memory-flag">{arrivalLabel(item.received_at, new Date())}</span>
      </p>
      {item.reason && <p className="inbox-reason">{item.reason}</p>}
      {item.memories.length > 0 && (
        <ul className="inbox-memories">
          {item.memories.map((m) => (
            <li key={m.id}>
              <button className="memory-link" onClick={() => onMemory(m.id)}>
                {m.summary}
              </button>
              {m.archived && <span className="memory-flag">archived</span>}
            </li>
          ))}
        </ul>
      )}
      <pre className="inbox-body">{item.body}</pre>
    </article>
  )
}
