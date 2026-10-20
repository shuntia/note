'use strict'

const test = require('node:test')
const assert = require('node:assert/strict')
const { pauseOthers } = require('./media')

function fakeBus(players) {
  const calls = []
  const run = async (cmd, args) => {
    assert.equal(cmd, 'gdbus')
    const dest = args[args.indexOf('--dest') + 1]
    const method = args[args.indexOf('--method') + 1]
    calls.push(`${dest} ${method}`)
    if (method === 'org.freedesktop.DBus.ListNames') {
      return `(['org.freedesktop.DBus', ':1.4', ${Object.keys(players).map((n) => `'${n}'`).join(', ')}],)\n`
    }
    if (method === 'org.freedesktop.DBus.Properties.Get') {
      if (players[dest] === 'gone') throw new Error('no such name')
      return `(<'${players[dest]}'>,)\n`
    }
    return '()\n'
  }
  return { run, calls }
}

const acts = (calls) => calls.filter((c) => /Player\.(Pause|Play)$/.test(c))

test('a call pauses only the players that were playing and resumes only those', async () => {
  const bus = fakeBus({
    'org.mpris.MediaPlayer2.spotify': 'Playing',
    'org.mpris.MediaPlayer2.vlc': 'Paused',
    'org.mpris.MediaPlayer2.firefox.instance_1_9': 'Playing',
    'org.mpris.MediaPlayer2.mpd': 'gone',
    'org.mpris.MediaPlayer2.chromium.instance42': 'Playing',
  })
  const resume = await pauseOthers({ platform: 'linux', run: bus.run, pid: 42 })
  assert.deepEqual(acts(bus.calls), [
    'org.mpris.MediaPlayer2.spotify org.mpris.MediaPlayer2.Player.Pause',
    'org.mpris.MediaPlayer2.firefox.instance_1_9 org.mpris.MediaPlayer2.Player.Pause',
  ])
  bus.calls.length = 0
  await resume()
  assert.deepEqual(acts(bus.calls), [
    'org.mpris.MediaPlayer2.spotify org.mpris.MediaPlayer2.Player.Play',
    'org.mpris.MediaPlayer2.firefox.instance_1_9 org.mpris.MediaPlayer2.Player.Play',
  ])
})

test('no session bus, no powershell, no osascript: the call goes on and resuming does nothing', async () => {
  const run = async () => {
    throw new Error('ENOENT')
  }
  for (const platform of ['linux', 'win32', 'darwin', 'freebsd']) {
    const resume = await pauseOthers({ platform, run })
    await resume()
  }
})

test('windows resumes the sessions its pause reported, by id', async () => {
  const seen = []
  const run = async (cmd, args, env) => {
    seen.push({ cmd, env })
    return seen.length === 1 ? 'Spotify.exe\r\nChrome\r\n' : ''
  }
  const resume = await pauseOthers({ platform: 'win32', run })
  await resume()
  assert.equal(seen[0].cmd, 'powershell.exe')
  assert.equal(seen[1].env.NOTE_PAUSED, 'Spotify.exe\nChrome')
})

test('macos resumes only when its pause found something playing', async () => {
  for (const [answer, plays] of [['ok\n', 2], ['\n', 1]]) {
    let n = 0
    const resume = await pauseOthers({ platform: 'darwin', run: async () => (++n === 1 ? answer : 'ok') })
    await resume()
    assert.equal(n, plays)
  }
})
