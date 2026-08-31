import DOMPurify from 'dompurify'
import { marked } from 'marked'
import { useEffect, useMemo, useRef, type MouseEvent } from 'react'

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
    button.textContent = 'copy'
    wrap.append(button)
  }
  return holder.innerHTML
}

function flash(button: HTMLButtonElement, label: string, timer: { current: number }) {
  button.textContent = label
  window.clearTimeout(timer.current)
  timer.current = window.setTimeout(() => {
    button.textContent = 'copy'
  }, 1500)
}

export function Markdown({ text }: { text: string }) {
  const html = useMemo(() => render(text), [text])
  const timer = useRef(0)

  useEffect(() => () => window.clearTimeout(timer.current), [])

  const copy = (e: MouseEvent<HTMLDivElement>) => {
    const button = (e.target as HTMLElement | null)?.closest('button.code-copy')
    if (!(button instanceof HTMLButtonElement)) return
    const code = button.parentElement?.querySelector('pre')?.textContent ?? ''
    if (!navigator.clipboard) {
      flash(button, "couldn't copy", timer)
      return
    }
    navigator.clipboard.writeText(code).then(
      () => flash(button, 'copied', timer),
      () => flash(button, "couldn't copy", timer),
    )
  }

  return <div className="prose" onClick={copy} dangerouslySetInnerHTML={{ __html: html }} />
}
