import { beforeEach, expect, test, vi } from 'vitest'
import { applyTheme, paintSky, storedPlace } from './theme'

function fakeRoot() {
  const attrs = new Map<string, string>()
  const props = new Map<string, string>()
  return {
    getAttribute: (k: string) => attrs.get(k) ?? null,
    setAttribute: (k: string, v: string) => void attrs.set(k, v),
    removeAttribute: (k: string) => void attrs.delete(k),
    style: {
      set colorScheme(v: string) { props.set('color-scheme', v) },
      setProperty: (k: string, v: string) => void props.set(k, v),
      getPropertyValue: (k: string) => props.get(k) ?? '',
      removeProperty: (k: string) => void props.delete(k),
    },
  }
}

beforeEach(() => {
  const store = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
  vi.stubGlobal('document', {
    documentElement: fakeRoot(),
    hidden: false,
    querySelector: () => null,
    addEventListener: () => {},
    removeEventListener: () => {},
  })
  vi.stubGlobal('window', { setInterval: () => 1, clearInterval: () => {} })
  vi.stubGlobal('getComputedStyle', () => ({ getPropertyValue: () => '' }))
})

test('a malformed stored place is no place', () => {
  localStorage.setItem('note.place', '{not json')
  expect(storedPlace()).toBeNull()
  localStorage.setItem('note.place', '{"lat":"34","lon":-118}')
  expect(storedPlace()).toBeNull()
  localStorage.setItem('note.place', 'null')
  expect(storedPlace()).toBeNull()
  localStorage.setItem('note.place', '{"lat":34.1,"lon":-118.2}')
  expect(storedPlace()).toEqual({ lat: 34.1, lon: -118.2 })
})

test('leaving Sky clears what it painted', () => {
  const root = document.documentElement
  applyTheme('sky')
  expect(root.style.getPropertyValue('--ink')).not.toBe('')
  expect(root.getAttribute('data-scheme')).not.toBeNull()
  applyTheme('dark')
  expect(root.style.getPropertyValue('--ink')).toBe('')
  expect(root.getAttribute('data-scheme')).toBeNull()
  expect(root.getAttribute('data-theme')).toBe('dark')
})

test('a late paint does not land on another theme', () => {
  const root = document.documentElement
  applyTheme('dark')
  paintSky()
  expect(root.style.getPropertyValue('--ink')).toBe('')
  expect(localStorage.getItem('note.sky')).toBeNull()
})
