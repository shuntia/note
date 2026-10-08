import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test } from 'vitest'
import { ComposeButton } from './ComposeButton'

const call = () => {}

test('an empty draft offers a call to Note', () => {
  const html = renderToStaticMarkup(<ComposeButton draft="  " busy={false} micBlocked={false} onCall={call} />)
  expect(html).toContain('type="button"')
  expect(html).toContain('aria-label="Call Note"')
})

test('text turns the button back into send', () => {
  const html = renderToStaticMarkup(<ComposeButton draft="hi" busy={false} micBlocked={false} onCall={call} />)
  expect(html).toContain('type="submit"')
  expect(html).toContain('aria-label="Send"')
})

test('without calls the empty composer keeps a disabled send', () => {
  const html = renderToStaticMarkup(<ComposeButton draft="" busy={false} micBlocked={false} />)
  expect(html).toContain('aria-label="Send"')
  expect(html).toContain('disabled=""')
})

test('a blocked mic is struck through and cannot be pressed', () => {
  const html = renderToStaticMarkup(<ComposeButton draft="" busy={false} micBlocked onCall={call} />)
  expect(html).toContain('aria-label="Microphone blocked"')
  expect(html).toContain('disabled=""')
  expect(html).toContain('d="M4 4l16 16"')
})

test('the mic waits while a reply is still coming', () => {
  const html = renderToStaticMarkup(<ComposeButton draft="" busy micBlocked={false} onCall={call} />)
  expect(html).toContain('aria-label="Call Note"')
  expect(html).toContain('disabled=""')
})
