// Usage: node scripts/shot.mjs <today|tasks|chat|memory|settings> <WxH> <out.png> [--session]
// Expects `pnpm dev` on http://localhost:5173 with NOTE_API pointing at the server this
// script starts (default http://127.0.0.1:3299).
import { chromium } from 'playwright-core'
import { spawn, execFileSync } from 'node:child_process'
import { mkdtempSync, writeFileSync, cpSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const [tab = 'today', size = '390x844', out = 'shot.png', ...flags] = process.argv.slice(2)
const [width, height] = size.split('x').map(Number)
const root = resolve(import.meta.dirname, '../..')
const port = Number(process.env.NOTE_PORT ?? 3299)

const dir = mkdtempSync(join(tmpdir(), 'note-shot-'))
cpSync(join(root, 'config/defaults'), join(dir, 'config/defaults'), { recursive: true })
writeFileSync(
  join(dir, 'config/server.toml'),
  `bind_addr = "127.0.0.1:${port}"\npublic_base_url = "http://127.0.0.1:${port}"\ndata_dir = "data"\n`,
)
const bin = join(root, 'target/debug/note-server')
execFileSync(bin, ['create-user', 'shot', 'shot-pass'], { cwd: dir, stdio: 'ignore' })
const server = spawn(bin, [], { cwd: dir, stdio: 'ignore' })
process.on('exit', () => server.kill())
await new Promise((r) => setTimeout(r, 800))

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? '/etc/profiles/per-user/user/bin/chromium' })
const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 })
const external = []
page.on('request', (req) => {
  const url = new URL(req.url())
  if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') external.push(req.url())
})
await page.goto('http://localhost:5173/')
await page.fill('input[placeholder="Username"]', 'shot')
await page.fill('input[placeholder="Password"]', 'shot-pass')
await page.click('button:has-text("Sign in")')
// Both the sidebar and the tab bar carry the label; CSS shows one per width.
const nav = 'nav[aria-label="Views"]:visible'
await page.waitForSelector(nav, { timeout: 10000 })
if (flags.includes('--session')) {
  await page.evaluate(() => {
    localStorage.setItem(
      'note.nowSession',
      JSON.stringify({
        taskId: null, eventId: null, title: 'Email landlord about the leak', notes: '',
        stepIndex: 2, stepCount: 3, stepName: 'photos of the ceiling',
        durationSec: 1500, startedAt: Date.now() - 492_000, pausedAt: null, pausedMs: 0,
      }),
    )
  })
  await page.reload()
  await page.waitForTimeout(500)
}
if (tab !== 'today') {
  await page.click(`${nav} button:has-text("${tab[0].toUpperCase()}${tab.slice(1)}")`)
  await page.waitForTimeout(500)
}
await page.waitForTimeout(800)
await page.screenshot({ path: out })
console.log(`wrote ${out}`)
if (external.length) {
  console.error(`external requests:\n${external.join('\n')}`)
  process.exitCode = 1
}
await browser.close()
server.kill()
