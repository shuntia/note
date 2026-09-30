import { en } from './en'

export type Key = keyof typeof en
/** A language file: every key English has, each a string. */
export type Dict = { readonly [K in Key]: string }
export type Vars = Record<string, string | number>

const DICTS: Record<string, Dict> = { en }

let locale = 'en'

export function activeLocale(): string {
  return locale
}

/** Switches to `tag` when a dictionary exists for it; English otherwise. */
export function setLocale(tag: string): void {
  locale = tag in DICTS ? tag : 'en'
}

/**
 * The string for `key` in the active language, with `{name}` placeholders filled
 * and `{count, one {…} other {…}}` choosing the plural form for `count`, where `#`
 * stands for the number.
 */
export function t(key: Key, vars: Vars = {}): string {
  return fill(DICTS[locale][key] ?? en[key], vars, locale)
}

export function fill(template: string, vars: Vars, tag = 'en'): string {
  let out = ''
  let i = 0
  while (i < template.length) {
    const open = template.indexOf('{', i)
    if (open === -1) return out + template.slice(i)
    out += template.slice(i, open)
    const close = matching(template, open)
    const inner = template.slice(open + 1, close)
    const comma = inner.indexOf(',')
    if (comma === -1) {
      const name = inner.trim()
      out += name in vars ? String(vars[name]) : `{${name}}`
    } else {
      const name = inner.slice(0, comma).trim()
      out += plural(inner.slice(comma + 1), Number(vars[name]), tag)
    }
    i = close + 1
  }
  return out
}

function matching(s: string, open: number): number {
  let depth = 0
  for (let i = open; i < s.length; i++) {
    if (s[i] === '{') depth++
    else if (s[i] === '}' && --depth === 0) return i
  }
  return s.length - 1
}

function plural(options: string, n: number, tag: string): string {
  const forms = new Map<string, string>()
  let i = 0
  while (i < options.length) {
    const open = options.indexOf('{', i)
    if (open === -1) break
    const close = matching(options, open)
    forms.set(options.slice(i, open).trim(), options.slice(open + 1, close))
    i = close + 1
  }
  const exact = forms.get(`=${n}`)
  const form = exact ?? forms.get(new Intl.PluralRules(tag).select(n)) ?? forms.get('other') ?? ''
  return form.replaceAll('#', new Intl.NumberFormat(tag).format(n))
}
