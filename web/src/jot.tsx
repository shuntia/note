import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type MouseEvent,
} from 'react'
import { api } from './api'
import { Markdown } from './markdown'
import { fold, settle, unfold } from './motion-gsap'
import { doing } from './receipts'
import { onAgentFrame, type AgentFrame } from './ws'

const DRAFT_KEY = 'note.captureDraft'
const PLACEHOLDER = 'Jot anything'
const TROUBLE = "Couldn't reach Note. Try again."

type Line =
  | { kind: 'said'; key: string; text: string }
  | { kind: 'reply'; key: string; text: string }
  | { kind: 'oops'; key: string; text: string }

// The call the live frames say is running, with `seq` the last frame applied.
type Live = { seq: number; step: { name: string; args: string } | null }

const QUIET: Live = { seq: -1, step: null }

function applyFrame(prev: Live, frame: AgentFrame): Live {
  if (frame.seq <= prev.seq) return prev
  const ev = frame.event
  if (ev.kind === 'tool_call') return { seq: frame.seq, step: { name: ev.name, args: ev.args } }
  if (ev.kind === 'tool_result') return { seq: frame.seq, step: null }
  return { ...prev, seq: frame.seq }
}

let sequence = 0
const nextKey = () => `jot-${++sequence}`

function readDraft(): string {
  try {
    return localStorage.getItem(DRAFT_KEY) ?? ''
  } catch {
    return ''
  }
}

function writeDraft(text: string) {
  try {
    if (text) localStorage.setItem(DRAFT_KEY, text)
    else localStorage.removeItem(DRAFT_KEY)
  } catch {
    // storage blocked; the draft still holds for this session
  }
}

// The shell's one always-there input: what it is given goes to Note as a message, and
// Note's answer unfolds under the box until something sends the panel away.
// `flow` puts the panel in the page rather than hanging it under the box; `tab` is the
// view around it, so a change of view closes the panel with it.
export function Jot({
  openTalk,
  openConversation,
  flow = false,
  tab,
}: {
  openTalk: (draft: string) => void
  openConversation: (id: number) => void
  flow?: boolean
  tab?: string
}) {
  const [text, setText] = useState(readDraft)
  const [lines, setLines] = useState<Line[]>([])
  const [conversation, setConversation] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [live, setLive] = useState<Live>(QUIET)

  const wrap = useRef<HTMLDivElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLInputElement>(null)
  // Where focus was when the shortcut stole it, so Esc can hand it back.
  const returnTo = useRef<HTMLElement | null>(null)
  // Lines already on screen, so only the ones that just arrived are settled in.
  const shown = useRef(0)
  // Bumped whenever the panel empties, so a late reply never lands in a new one.
  const era = useRef(0)
  const folding = useRef(false)
  // The send in flight, for the frames arriving on the shell's socket.
  const inFlight = useRef<{ conversation: number | null } | null>(null)

  const open = lines.length > 0

  const clear = useCallback(() => {
    era.current++
    inFlight.current = null
    setLines([])
    setConversation(null)
    setLive(QUIET)
    setBusy(false)
  }, [])

  const close = useCallback(() => {
    const el = panel.current
    if (!el || folding.current) return
    folding.current = true
    fold(el, () => {
      folding.current = false
      clear()
    })
  }, [clear])

  useLayoutEffect(() => {
    const el = panel.current
    if (!el) {
      shown.current = 0
      return
    }
    const rows = [...el.querySelectorAll<HTMLElement>('.jot-line')]
    if (shown.current === 0) unfold(el)
    else settle(rows.slice(shown.current))
    shown.current = rows.length
  }, [lines])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'n' || e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return
      const el = e.target as HTMLElement | null
      const tag = el?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el?.isContentEditable) return
      e.preventDefault()
      returnTo.current = el
      input.current?.focus()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  useEffect(() => {
    if (!open) return
    const onDown = (e: PointerEvent) => {
      if (!wrap.current?.contains(e.target as Node)) close()
    }
    document.addEventListener('pointerdown', onDown)
    return () => document.removeEventListener('pointerdown', onDown)
  }, [open, close])

  const view = useRef(tab)
  useEffect(() => {
    if (view.current === tab) return
    view.current = tab
    close()
  }, [tab, close])

  useEffect(
    () =>
      onAgentFrame((frame) => {
        const sent = inFlight.current
        if (!sent) return
        if (frame.conversation_id !== null && frame.conversation_id !== sent.conversation) return
        setLive((prev) => applyFrame(prev, frame))
      }),
    [],
  )

  const change = (value: string) => {
    setText(value)
    writeDraft(value)
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const message = text.trim()
    if (!message || busy) return
    const sentIn = era.current
    const thread = conversation
    change('')
    setLive(QUIET)
    setBusy(true)
    inFlight.current = { conversation: thread }
    setLines((prev) => [...prev, { kind: 'said', key: nextKey(), text: message }])
    try {
      const answer = await api.talk(message, thread ?? undefined)
      if (era.current !== sentIn) return
      setConversation(answer.conversation_id)
      setLines((prev) => [...prev, { kind: 'reply', key: nextKey(), text: answer.reply }])
    } catch {
      if (era.current !== sentIn) return
      setLines((prev) => [...prev, { kind: 'oops', key: nextKey(), text: TROUBLE }])
      change(message)
    } finally {
      if (era.current === sentIn) {
        inFlight.current = null
        setBusy(false)
      }
    }
  }

  // The panel is a foretaste of the thread; opening it hands the conversation to Chat.
  const openThread = () => {
    const said = lines.find((l) => l.kind === 'said')
    const id = conversation
    clear()
    if (id !== null) openConversation(id)
    else openTalk(said?.text ?? '')
  }

  const onPanelClick = (e: MouseEvent) => {
    if ((e.target as HTMLElement).closest('a, button')) return
    openThread()
  }

  const label = !busy
    ? null
    : live.step
      ? `${doing(live.step.name, live.step.args)}…`
      : 'Note is thinking…'

  return (
    <div className={`jot-wrap${flow ? ' flow' : ''}`} ref={wrap}>
      <form className={`jot${open ? ' open' : ''}`} onSubmit={submit}>
        {!flow && (
          <span className="jot-glyph" aria-hidden="true">
            +
          </span>
        )}
        <input
          ref={input}
          value={text}
          aria-label={PLACEHOLDER}
          placeholder={PLACEHOLDER}
          onChange={(e) => change(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== 'Escape') return
            if (open) {
              close()
              return
            }
            e.currentTarget.blur()
            returnTo.current?.focus()
            returnTo.current = null
          }}
        />
        {flow ? (
          <button
            className="jot-send"
            type="submit"
            aria-label="Send"
            disabled={busy || !text.trim()}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M5 12h14" />
              <path d="M13 6l6 6-6 6" />
            </svg>
          </button>
        ) : (
          <kbd className="jot-key" aria-hidden="true">
            N
          </kbd>
        )}
      </form>
      {open && (
        <div
          className="jot-panel"
          ref={panel}
          role="button"
          tabIndex={0}
          aria-label="Open in Chat"
          onClick={onPanelClick}
          onKeyDown={(e) => {
            if (e.key !== 'Enter') return
            e.preventDefault()
            openThread()
          }}
        >
          {lines.map((line) => {
            if (line.kind === 'said')
              return (
                <p key={line.key} className="jot-line jot-said">
                  {line.text}
                </p>
              )
            if (line.kind === 'oops')
              return (
                <p key={line.key} className="jot-line jot-oops" role="alert">
                  {line.text}
                </p>
              )
            return (
              <div key={line.key} className="jot-line jot-reply">
                <Markdown text={line.text} />
              </div>
            )
          })}
          {label && (
            <p className="jot-think" aria-live="polite">
              <span className="jot-dot" aria-hidden="true" />
              {label}
            </p>
          )}
          <span className="jot-hint" aria-hidden="true">
            Open in Chat ›
          </span>
        </div>
      )}
    </div>
  )
}
