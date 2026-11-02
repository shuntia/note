'use strict'

const test = require('node:test')
const assert = require('node:assert/strict')
const { LAUNCHER_HEIGHT, centerTop, clampZoom, cornerBounds, launcherHeight, micSettingsUrl, ringInfo, spellLanguages } = require('./shell')

test('a ring without details is an old page that wants the window', () => {
  assert.equal(ringInfo(undefined), null)
  assert.equal(ringInfo('x'), null)
  assert.deepEqual(ringInfo({}), { title: null })
  assert.deepEqual(ringInfo({ title: '  Morning check-in ' }), { title: 'Morning check-in' })
  assert.deepEqual(ringInfo({ title: 42 }), { title: null })
  assert.equal(ringInfo({ title: 'あ'.repeat(200) }).title.length, 80)
})

test('the popup sits in the top-right corner of the work area', () => {
  const area = { x: 100, y: 30, width: 1920, height: 1050 }
  assert.deepEqual(cornerBounds(area, { width: 300, height: 90 }, 16), { x: 1704, y: 46, width: 300, height: 90 })
})

test('mic privacy settings exist on macOS and Windows only', () => {
  assert.match(micSettingsUrl('darwin'), /^x-apple\.systempreferences:.*Privacy_Microphone$/)
  assert.equal(micSettingsUrl('win32'), 'ms-settings:privacy-microphone')
  assert.equal(micSettingsUrl('linux'), null)
})

test('the launcher sits centred in the upper third', () => {
  const area = { x: 0, y: 24, width: 1920, height: 1056 }
  assert.deepEqual(centerTop(area, { width: 640, height: 72 }), { x: 640, y: 200, width: 640, height: 72 })
})

test('the launcher grows with its reply up to a cap', () => {
  assert.equal(launcherHeight(10), LAUNCHER_HEIGHT.min)
  assert.equal(launcherHeight(300.2), 301)
  assert.equal(launcherHeight(5000), LAUNCHER_HEIGHT.max)
  assert.equal(launcherHeight(NaN), LAUNCHER_HEIGHT.min)
})

test('zoom stays within a readable range', () => {
  assert.equal(clampZoom(9), 5)
  assert.equal(clampZoom(-9), -3)
  assert.equal(clampZoom(1.5), 1.5)
  assert.equal(clampZoom(undefined), 0)
})

test('spellcheck takes the wanted languages the platform has', () => {
  assert.deepEqual(spellLanguages(['en-GB', 'en-US', 'fr'], ['en-US', 'ja']), ['en-US'])
  assert.deepEqual(spellLanguages(['en-GB', 'ja-JP'], ['en-US', 'ja']), ['en-GB', 'ja-JP'])
  assert.deepEqual(spellLanguages([], ['en-US']), [])
})
