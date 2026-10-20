'use strict'

const { execFile } = require('node:child_process')

const TIMEOUT_MS = 4000

function exec(cmd, args, env) {
  return new Promise((resolve, reject) => {
    execFile(cmd, args, { timeout: TIMEOUT_MS, windowsHide: true, env: { ...process.env, ...env } }, (error, stdout) =>
      error ? reject(error) : resolve(String(stdout)),
    )
  })
}

const MPRIS = 'org.mpris.MediaPlayer2'
const gdbus = (dest, method, ...args) =>
  ['call', '--session', '--dest', dest, '--object-path', dest === 'org.freedesktop.DBus' ? '/org/freedesktop/DBus' : '/org/mpris/MediaPlayer2', '--method', method, ...args]

// `skip` is this app's own player, which Chromium registers as chromium.instance<pid>.
async function pauseMpris(run, skip) {
  const names = await run('gdbus', gdbus('org.freedesktop.DBus', 'org.freedesktop.DBus.ListNames'))
  const players = [...names.matchAll(/'(org\.mpris\.MediaPlayer2\.[^']+)'/g)].map((m) => m[1]).filter((n) => !n.endsWith(skip))
  const paused = []
  for (const name of players) {
    try {
      const status = await run('gdbus', gdbus(name, 'org.freedesktop.DBus.Properties.Get', `${MPRIS}.Player`, 'PlaybackStatus'))
      if (!status.includes("'Playing'")) continue
      await run('gdbus', gdbus(name, `${MPRIS}.Player.Pause`))
      paused.push(name)
    } catch {}
  }
  return async () => {
    for (const name of paused) await run('gdbus', gdbus(name, `${MPRIS}.Player.Play`)).catch(() => {})
  }
}

const SMTC_PRELUDE = `
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation\`1' } | Select-Object -First 1
function Await($op, [Type]$type) { $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op)); $t.Wait(-1) | Out-Null; $t.Result }
$mgrType = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager, Windows.Media.Control, ContentType = WindowsRuntime]
$mgr = Await ($mgrType::RequestAsync()) $mgrType
`
const SMTC_PAUSE = `${SMTC_PRELUDE}
foreach ($s in $mgr.GetSessions()) {
  if ($s.GetPlaybackInfo().PlaybackStatus -eq 'Playing' -and (Await ($s.TryPauseAsync()) ([bool]))) { $s.SourceAppUserModelId }
}`
const SMTC_PLAY = `${SMTC_PRELUDE}
$ids = $env:NOTE_PAUSED -split "\`n"
foreach ($s in $mgr.GetSessions()) {
  if ($ids -contains $s.SourceAppUserModelId) { Await ($s.TryPlayAsync()) ([bool]) | Out-Null }
}`

async function pauseSmtc(run) {
  const ps = (script, env) => run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], env)
  const paused = (await ps(SMTC_PAUSE)).split(/\r?\n/).map((s) => s.trim()).filter(Boolean)
  return async () => {
    if (paused.length) await ps(SMTC_PLAY, { NOTE_PAUSED: paused.join('\n') }).catch(() => {})
  }
}

// MediaRemote is private and gated since macOS 15.4; JXA through osascript still
// reaches it (the route upnext-adapter-nowplaying takes, Apache-2.0).
const MEDIA_REMOTE = `
ObjC.import('Foundation')
function remote() {
  var bundle = $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/')
  if (!bundle || !bundle.load) return null
  return $.NSClassFromString('MRNowPlayingRequest') || null
}
function send(R, code) {
  var path = R.localNowPlayingPlayerPath
  if (!path || path.isNil()) return ''
  R.alloc.initWithPlayerPath(path).sendCommandOptionsQueueCompletion(code, $.NSDictionary.dictionary, $.NSOperationQueue.mainQueue.underlyingQueue, $())
  return 'ok'
}`
const MR_PAUSE = `${MEDIA_REMOTE}
function run() { var R = remote(); return R && R.localIsPlaying === true ? send(R, 1) : '' }`
const MR_PLAY = `${MEDIA_REMOTE}
function run() { var R = remote(); return R ? send(R, 0) : '' }`

async function pauseMediaRemote(run) {
  const jxa = (script) => run('osascript', ['-l', 'JavaScript', '-e', script])
  const paused = (await jxa(MR_PAUSE)).trim() === 'ok'
  return async () => {
    if (paused) await jxa(MR_PLAY).catch(() => {})
  }
}

/** Pauses whatever else is playing and answers how to resume exactly that; a platform without a way answers a no-op. */
async function pauseOthers({ platform = process.platform, run = exec, pid = process.pid } = {}) {
  const none = async () => {}
  try {
    if (platform === 'linux') return await pauseMpris(run, `instance${pid}`)
    if (platform === 'win32') return await pauseSmtc(run)
    if (platform === 'darwin') return await pauseMediaRemote(run)
  } catch {}
  return none
}

module.exports = { pauseOthers }
