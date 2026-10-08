// Usage: node scripts/call-check.mjs
// Opens a web call in headless chromium with a fake microphone, against a fake voice side, and checks
// that it reaches listening, carries the caller's audio, draws the circle and ends cleanly.
// Expects `pnpm dev` on http://localhost:5173 with NOTE_API=http://127.0.0.1:3299 and a built
// target/debug/note-server.
import { chromium } from 'playwright-core'
import { spawn, execFileSync } from 'node:child_process'
import { mkdtempSync, writeFileSync, cpSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fakeVoice } from './fake-voice.mjs'

const root = resolve(import.meta.dirname, '../..')
const port = Number(process.env.NOTE_PORT ?? 3299)
const dir = mkdtempSync(join(tmpdir(), 'note-call-'))
const socket = join(dir, 'voice.sock')
cpSync(join(root, 'config/defaults'), join(dir, 'config/defaults'), { recursive: true })
writeFileSync(
  join(dir, 'config/server.toml'),
  `bind_addr = "127.0.0.1:${port}"\npublic_base_url = "http://127.0.0.1:${port}"\ndata_dir = "data"\n\n[voice]\nsocket = "${socket}"\n`,
)
const bin = join(root, 'target/debug/note-server')
execFileSync(bin, ['create-user', 'aitest', 'aitest-pass', '--test'], { cwd: dir, stdio: 'ignore' })
const server = spawn(bin, [], { cwd: dir, stdio: 'ignore' })
let finished = false
let voice = null
let browser = null
const cleanup = () => {
  voice?.close()
  server.kill()
}
const fail = async (why) => {
  finished = true
  console.error(`call check failed: ${why}`)
  await browser?.close().catch(() => {})
  cleanup()
  process.exit(1)
}
server.on('exit', (code) => {
  if (!finished) void fail(`note-server exited early (code ${code}) - is port ${port} already in use?`)
})
const waitFor = async (what, ok, ms = 10_000) => {
  const until = Date.now() + ms
  while (!ok()) {
    if (Date.now() > until) await fail(`timed out waiting for ${what}`)
    await new Promise((r) => setTimeout(r, 50))
  }
}

await new Promise((r) => setTimeout(r, 800))
voice = fakeVoice(socket)
await waitFor('the voice link', () => voice.state.up)

browser = await chromium.launch({
  executablePath: process.env.CHROMIUM ?? execFileSync('sh', ['-c', 'command -v chromium']).toString().trim(),
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required'],
})
const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
await context.grantPermissions(['microphone'], { origin: 'http://localhost:5173' })
const page = await context.newPage()

await page.goto('http://localhost:5173/')
await page.fill('input[placeholder="Username"]', 'aitest')
await page.fill('input[placeholder="Password"]', 'aitest-pass')
await page.click('button:has-text("Sign in")')
await page.waitForSelector('.shell', { timeout: 10_000 })
// Errors count from sign-in on: the signed-out page's session probe answers 401 by design.
const errors = []
page.on('pageerror', (e) => errors.push(String(e)))
page.on('console', (m) => {
  if (m.type() === 'error') errors.push(m.text())
})
await page.click('nav[aria-label="Views"]:visible button:has-text("Chat")')
await page.click('button[aria-label="Call Note"]', { timeout: 10_000 })
await page.waitForSelector('.call-view[data-state="listening"]', { timeout: 10_000 })
await waitFor("the caller's audio at the voice side", () => voice.state.audioIn >= 25)
const inked = await page.evaluate(() => {
  const c = document.querySelector('.call-view canvas')
  const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data
  let n = 0
  for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++
  return n
})
if (inked === 0) await fail('the circle was not drawn')
await page.click('button[aria-label="End call"]')
await waitFor('the hang-up at the voice side', () => voice.state.hungUp.length >= 1)
await page.waitForSelector('.call-view', { state: 'detached', timeout: 5000 })
if (voice.state.calls.length !== 1) await fail(`expected one call, saw ${voice.state.calls.length}`)
if (errors.length) await fail(`page errors:\n${errors.join('\n')}`)
console.log('call check passed')
finished = true
await browser.close()
cleanup()
