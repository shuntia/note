import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test } from 'vitest'
import { declineFrame, visibilityFrame } from '../ws'
import { CallView } from './CallView'

const none = () => {}

test('a ringing call is answered at the circle and declined at the cross', () => {
  const html = renderToStaticMarkup(<CallView call={{ conversationId: 3, ring: 'r', at: 1 }} onClose={none} onMicBlocked={none} />)
  expect(html).toContain('data-state="ringing"')
  expect(html).toContain('aria-label="Answer"')
  expect(html).toContain('aria-label="End call"')
})

test('a call the user starts opens connecting and unmuted', () => {
  const html = renderToStaticMarkup(<CallView call={{ conversationId: null, ring: null, at: 1 }} onClose={none} onMicBlocked={none} />)
  expect(html).toContain('data-state="connecting"')
  expect(html).toContain('aria-label="Mute"')
  expect(html).toContain('aria-pressed="false"')
})

test('a page tells the server whether it can be rung', () => {
  expect(JSON.parse(visibilityFrame(true))).toEqual({ type: 'visible', on: true })
  expect(JSON.parse(visibilityFrame(false))).toEqual({ type: 'visible', on: false })
})

test('a declined ring names its ring to the server', () => {
  expect(JSON.parse(declineFrame('r1'))).toEqual({ type: 'decline', ring: 'r1' })
})
