import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from 'vitest'

const SRC = fileURLToPath(new URL('..', import.meta.url))

function sources(dir: string): string[] {
  return readdirSync(join(SRC, dir), { withFileTypes: true }).flatMap((e) => {
    const path = dir ? `${dir}/${e.name}` : e.name
    if (e.isDirectory()) return path === 'i18n' ? [] : sources(path)
    return /\.tsx?$/.test(e.name) && !/\.test\./.test(e.name) ? [path] : []
  })
}

const COVERED = sources('')

const WORD = /[A-Za-z]{3,}/

// Text a reader sees: JSX text, the attributes that are read aloud or shown, the
// arguments of a toast and the labels of menu items.
const PATTERNS: [string, RegExp][] = [
  ['jsx text', /(?<![=-])(?<!\S )>([^<>{}=;]*)</g],
  ['jsx text', /(?<![=-])(?<!\S )>([^<>{}=;]*)\{/g],
  ['jsx text', /\}([^<>{}=;()]*)<\//g],
  ['choice', /[?:]\s*(['"`])([A-Z][a-z](?:(?!\1).)*)\1/g],
  ['attribute', /\b(?:aria-label|placeholder|title|alt|label)="([^"]*)"/g],
  ['attribute', /\b(?:aria-label|placeholder|title|alt|label)=\{`([^`]*)`\}/g],
  ['sentence', /(['"`])((?:(?!\1).)*\b(?:Couldn't|Try again|didn't|Nothing)\b(?:(?!\1).)*)\1/g],
  ['toast', /\bnotify\(\s*(['"`])((?:(?!\1).)*)\1/g],
  ['menu label', /\blabel:\s*(['"`])((?:(?!\1).)*)\1/g],
  ['error', /\bset(?:Error|Failed)\(\s*(['"`])((?:(?!\1).)*)\1/g],
  ['return text', /\breturn\s+(['"`])((?:(?!\1).)*[a-z]{3,}(?:(?!\1).)*)\1/g],
]

const ALLOWED = /^(?:\s|&[a-z]+;|[0-9:·–—…%+×/.,-])*$|^[a-z]+(?:\.[a-zA-Z]+)+$/

function findings(path: string): string[] {
  const src = readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/^\s*\/\/.*$/gm, '')
  const out: string[] = []
  for (const [kind, re] of PATTERNS) {
    for (const m of src.matchAll(re)) {
      const text = m[m.length - 1].replace(/\$\{[^}]*\}?/g, ' ')
      if (!WORD.test(text) || ALLOWED.test(text)) continue
      if (kind === 'return text' && !/\s[a-z]|^[A-Z]/.test(text.trim())) continue
      if (kind === 'jsx text' && (/[()]/.test(text) || !/[A-Za-z]{3,}\s+[A-Za-z]|^\s*[A-Z][a-z]{2,}/.test(text.trim()))) continue
      const line = src.slice(0, m.index).split('\n').length
      out.push(`${path}:${line} ${kind}: ${text.trim().slice(0, 60)}`)
    }
  }
  return out
}

test('every word a reader sees in the covered views comes from the dictionary', () => {
  expect(COVERED.flatMap(findings)).toEqual([])
})
