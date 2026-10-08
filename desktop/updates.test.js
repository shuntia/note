'use strict'

const test = require('node:test')
const assert = require('node:assert/strict')
const { updateMode } = require('./updates')

const packaged = { packaged: true, dev: false }

test('Windows installs and AppImages update themselves', () => {
  assert.equal(updateMode({ ...packaged, platform: 'win32' }), 'self')
  assert.equal(updateMode({ ...packaged, platform: 'linux', env: { APPIMAGE: '/opt/Note.AppImage' } }), 'self')
})

test('macOS and the deb point at the releases page', () => {
  assert.equal(updateMode({ ...packaged, platform: 'darwin' }), 'page')
  assert.equal(updateMode({ ...packaged, platform: 'linux' }), 'page')
})

test('dev runs, checkouts and the Nix build never update', () => {
  assert.equal(updateMode({ packaged: true, dev: true, platform: 'win32' }), null)
  assert.equal(updateMode({ packaged: false, dev: false, platform: 'linux', env: { APPIMAGE: '/x' } }), null)
  assert.equal(updateMode({ ...packaged, platform: 'linux', env: { NOTE_DESKTOP_EXEC: '/nix/store/x/bin/note-desktop' } }), null)
})
