'use strict'

const test = require('node:test')
const assert = require('node:assert/strict')
const { normalizeServerUrl, resolveServerUrl } = require('./server-url')

test('normalizes http(s) addresses and rejects the rest', () => {
  assert.equal(normalizeServerUrl('https://note.example.com'), 'https://note.example.com/')
  assert.equal(normalizeServerUrl('  note.example.com  '), 'https://note.example.com/')
  assert.equal(normalizeServerUrl('http://127.0.0.1:3271'), 'http://127.0.0.1:3271/')
  assert.equal(normalizeServerUrl(''), null)
  assert.equal(normalizeServerUrl(undefined), null)
  assert.equal(normalizeServerUrl('ftp://note.example.com'), null)
  assert.equal(normalizeServerUrl('file:///etc/passwd'), null)
  assert.equal(normalizeServerUrl('https://'), null)
})

test('resolves --url, then NOTE_URL, then the saved setting, then the baked one', () => {
  const all = {
    argv: ['electron', '.', '--url=https://arg.example'],
    env: { NOTE_URL: 'https://env.example' },
    saved: 'https://saved.example',
    baked: 'https://baked.example',
  }
  assert.deepEqual(resolveServerUrl(all), { url: 'https://arg.example/', source: 'arg' })
  assert.deepEqual(resolveServerUrl({ ...all, argv: [] }), { url: 'https://env.example/', source: 'env' })
  assert.deepEqual(resolveServerUrl({ ...all, argv: [], env: {} }), { url: 'https://saved.example/', source: 'saved' })
  assert.deepEqual(resolveServerUrl({ argv: [], env: {}, baked: 'https://baked.example' }), {
    url: 'https://baked.example/',
    source: 'baked',
  })
})

test('skips unusable values and yields null when nothing is set', () => {
  assert.deepEqual(resolveServerUrl({ argv: ['--url='], env: { NOTE_URL: 'ftp://x' }, saved: 'https://saved.example' }), {
    url: 'https://saved.example/',
    source: 'saved',
  })
  assert.equal(resolveServerUrl({}), null)
})
