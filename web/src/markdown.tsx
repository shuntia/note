import DOMPurify from 'dompurify'
import { marked } from 'marked'
import { useEffect, useMemo, useRef, type MouseEvent } from 'react'
import { t } from './i18n'

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A') {
    node.setAttribute('target', '_blank')
    node.setAttribute('rel', 'noreferrer')
  }
})

// Wrapping happens on the sanitized markup, before React owns the subtree.
function render(text: string): string {
  const holder = document.createElement('div')
  holder.innerHTML = DOMPurify.sanitize(marked(text, { gfm: true, breaks: true, async: false }))
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

export function Markdown({ text }: { text: string }) {
  const html = useMemo(() => render(text), [text])
  const timers = useRef(new Map<HTMLButtonElement, number>())

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

  return <div className="prose" onClick={copy} dangerouslySetInnerHTML={{ __html: html }} />
}
