import DOMPurify from 'dompurify'
import { marked } from 'marked'
import { useEffect, useLayoutEffect, useMemo, useRef, type MouseEvent } from 'react'
import { t } from './i18n'
import { Fader } from './streamfade'

// Model output carries text from the web and from strangers, so nothing in it may load on its own.
const PURIFY = {
  USE_PROFILES: { html: true },
  FORBID_TAGS: [
    'img', 'picture', 'source', 'video', 'audio', 'track', 'iframe', 'frame', 'object', 'embed',
    'form', 'button', 'textarea', 'select', 'option', 'style', 'link', 'meta', 'base',
  ],
  FORBID_ATTR: ['style', 'srcset', 'background', 'poster', 'action', 'formaction', 'ping'],
  SANITIZE_NAMED_PROPS: true,
}

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A') {
    node.setAttribute('target', '_blank')
    node.setAttribute('rel', 'noopener noreferrer')
  }
  if (node.tagName === 'INPUT') {
    node.setAttribute('type', 'checkbox')
    node.setAttribute('disabled', '')
  }
})

// Wrapping happens on the sanitized markup, before React owns the subtree.
function render(text: string): string {
  const holder = document.createElement('div')
  holder.innerHTML = DOMPurify.sanitize(marked(text, { gfm: true, breaks: true, async: false }), PURIFY)
  for (const pre of holder.querySelectorAll('pre')) {
    const wrap = document.createElement('div')
    wrap.className = 'code-block'
    pre.replaceWith(wrap)
    wrap.append(pre)
    const button = document.createElement('button')
    button.type = 'button'
    button.className = 'code-copy'
    button.textContent = t('markdown.copy')
    wrap.append(button)
  }
  return holder.innerHTML
}

/** `stream` fades text in as it grows. */
export function Markdown({ text, stream = false }: { text: string; stream?: boolean }) {
  const html = useMemo(() => render(text), [text])
  const timers = useRef(new Map<HTMLButtonElement, number>())
  const root = useRef<HTMLDivElement>(null)
  const fader = useRef<Fader | null>(null)

  useLayoutEffect(() => {
    if (!stream) return
    const f = new Fader(root.current!, 'rendered')
    fader.current = f
    return () => {
      f.dispose()
      fader.current = null
    }
  }, [stream])

  // eslint-disable-next-line react-hooks/exhaustive-deps
  useLayoutEffect(() => fader.current?.update(text), [html, stream])

  useEffect(() => {
    const live = timers.current
    return () => {
      for (const id of live.values()) window.clearTimeout(id)
    }
  }, [])

  const flash = (button: HTMLButtonElement, label: string) => {
    button.textContent = label
    window.clearTimeout(timers.current.get(button))
    const id = window.setTimeout(() => {
      button.textContent = t('markdown.copy')
      timers.current.delete(button)
    }, 1500)
    timers.current.set(button, id)
  }

  const copy = (e: MouseEvent<HTMLDivElement>) => {
    const button = (e.target as HTMLElement | null)?.closest('button.code-copy')
    if (!(button instanceof HTMLButtonElement)) return
    const code = button.parentElement?.querySelector('pre')?.textContent ?? ''
    if (!navigator.clipboard) {
      flash(button, t('markdown.copyFailed'))
      return
    }
    navigator.clipboard.writeText(code).then(
      () => flash(button, t('markdown.copied')),
      () => flash(button, t('markdown.copyFailed')),
    )
  }

  return <div className="prose" ref={root} onClick={copy} dangerouslySetInnerHTML={{ __html: html }} />
}
