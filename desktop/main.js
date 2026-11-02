'use strict'

const {
  app,
  BrowserWindow,
  Menu,
  Notification,
  Tray,
  clipboard,
  globalShortcut,
  ipcMain,
  nativeImage,
  net,
  powerMonitor,
  powerSaveBlocker,
  screen,
  shell,
  systemPreferences,
} = require('electron')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { pauseOthers } = require('./media')
const { normalizeServerUrl, resolveServerUrl } = require('./server-url')
const { LAUNCHER_HEIGHT, centerTop, clampZoom, cornerBounds, launcherHeight, micSettingsUrl, ringInfo, spellLanguages } = require('./shell')
const { CHECK_EVERY_MS, RELEASES_URL, updateMode } = require('./updates')

const BAKED_URL = require('./package.json').noteUrl
const ICONS = path.join(__dirname, 'icons')
const BACKGROUND = '#f0e8de'
const dev = process.argv.includes('--dev')

let server = null
let appUrl = null
let win = null
let setupWin = null
let widgetWin = null
let widgetClock = null
let ringWin = null
let ringShown = null
let ringLimit = null
let ringBounce = null
let launcherWin = null
let pendingOpen = null
let openReady = false
let callHold = null
let tray = null
let quitting = false
let failures = 0
let updater = null
let updateReady = false

const STRINGS = {
  en: {
    show: 'Show',
    hide: 'Hide',
    startAtLogin: 'Start at login',
    reload: 'Reload',
    forceReload: 'Force Reload',
    quit: 'Quit',
    file: 'File',
    edit: 'Edit',
    view: 'View',
    window: 'Window',
    close: 'Close',
    changeServer: 'Change server…',
    nightly: 'Nightly builds',
    checkUpdates: 'Check for updates',
    restartToUpdate: 'Restart to update',
    settings: 'Settings…',
    trayHint: 'Note keeps running in the tray.',
    openLink: 'Open in browser',
    copyLink: 'Copy link',
    addToDictionary: 'Add to dictionary',
    resetZoom: 'Actual Size',
    zoomIn: 'Zoom In',
    zoomOut: 'Zoom Out',
  },
  ja: {
    show: '表示',
    hide: '隠す',
    startAtLogin: 'ログイン時に起動',
    reload: '再読み込み',
    forceReload: '強制再読み込み',
    quit: '終了',
    file: 'ファイル',
    edit: '編集',
    view: '表示',
    window: 'ウインドウ',
    close: '閉じる',
    changeServer: 'サーバーを変更…',
    nightly: 'ナイトリービルド',
    checkUpdates: 'アップデートを確認',
    restartToUpdate: '再起動してアップデート',
    settings: '設定…',
    trayHint: 'Noteはトレイで動作を続けます。',
    openLink: 'ブラウザで開く',
    copyLink: 'リンクをコピー',
    addToDictionary: '辞書に追加',
    undo: '取り消す',
    redo: 'やり直す',
    cut: '切り取り',
    copy: 'コピー',
    paste: '貼り付け',
    selectAll: 'すべて選択',
    toggleDevTools: '開発者ツール',
    resetZoom: '実際のサイズ',
    zoomIn: '拡大',
    zoomOut: '縮小',
    togglefullscreen: 'フルスクリーン',
    minimize: '最小化',
    zoom: 'ズーム',
  },
}

// The page's own language once it has loaded (it follows the account's
// setting), the system's before that.
let lang = 'en'

function pickLang(tag) {
  const base = String(tag || '').toLowerCase().split('-')[0]
  return base in STRINGS ? base : 'en'
}

const s = (key) => STRINGS[lang][key] ?? STRINGS.en[key]

// A role item with the label for the current language, where Electron's own is English.
const role = (name) => (STRINGS[lang][name] ? { role: name, label: STRINGS[lang][name] } : { role: name })

const statePath = () => path.join(app.getPath('userData'), 'window-state.json')
const configPath = () => path.join(app.getPath('userData'), 'config.json')

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'))
  } catch {
    return {}
  }
}

function patchJson(file, patch) {
  const next = { ...readJson(file), ...patch }
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, JSON.stringify(next))
  } catch {}
}

const readState = () => readJson(statePath())
const writeState = (patch) => patchJson(statePath(), patch)

function visibleBounds(bounds) {
  if (!bounds) return undefined
  const area = screen.getDisplayMatching(bounds).workArea
  const overlaps =
    bounds.x < area.x + area.width &&
    bounds.x + bounds.width > area.x &&
    bounds.y < area.y + area.height &&
    bounds.y + bounds.height > area.y
  return overlaps ? bounds : { width: bounds.width, height: bounds.height }
}

const autostartFile = path.join(os.homedir(), '.config', 'autostart', 'note.desktop')

function desktopExecQuote(arg) {
  return /[\s"'\\$`]/.test(arg) ? `"${arg.replace(/(["\\$`])/g, '\\$1')}"` : arg
}

function profileWrapper(name) {
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    const candidate = path.join(dir, name)
    if (dir && !dir.startsWith('/nix/store/') && fs.existsSync(candidate)) return candidate
  }
  return undefined
}

// The command that relaunches this build: the AppImage, the Nix wrapper (via a
// profile's bin/ when installed, so it survives garbage collection), the
// packaged binary, or `electron <app dir>` from a checkout.
function launchCommand() {
  if (process.env.APPIMAGE) return [process.env.APPIMAGE]
  const nixExec = process.env.NOTE_DESKTOP_EXEC
  if (nixExec) return [profileWrapper(path.basename(nixExec)) || nixExec]
  if (app.isPackaged) return [process.execPath]
  return [process.execPath, app.getAppPath()]
}

function autostartEntry() {
  const extra = server && (server.source === 'arg' || server.source === 'env') ? [`--url=${server.url}`] : []
  const exec = [...launchCommand(), '--hidden', ...extra].map(desktopExecQuote).join(' ')
  return [
    '[Desktop Entry]',
    'Type=Application',
    'Name=Note',
    `Exec=${exec}`,
    'X-GNOME-Autostart-enabled=true',
    'NoDisplay=true',
    '',
  ].join('\n')
}

function autostartEnabled() {
  if (process.platform === 'linux') return fs.existsSync(autostartFile)
  return app.getLoginItemSettings().openAtLogin
}

function setAutostart(on) {
  if (process.platform === 'linux') {
    if (on) {
      fs.mkdirSync(path.dirname(autostartFile), { recursive: true })
      fs.writeFileSync(autostartFile, autostartEntry())
    } else {
      fs.rmSync(autostartFile, { force: true })
    }
    return
  }
  app.setLoginItemSettings({ openAtLogin: on, args: ['--hidden'] })
}

// Enabled on first run; afterwards an existing Linux entry is rewritten so it
// follows the current build path (Nix store paths change on every update).
function initAutostart() {
  if (!readState().autostartInitialized) {
    setAutostart(true)
    writeState({ autostartInitialized: true })
  } else if (process.platform === 'linux' && autostartEnabled()) {
    setAutostart(true)
  }
}

const sameOrigin = (url) => {
  if (!appUrl) return false
  try {
    return new URL(url).origin === appUrl.origin
  } catch {
    return false
  }
}

function openExternal(url) {
  if (/^(https?|mailto):/.test(url)) shell.openExternal(url)
}

// Failures are handled by the did-fail-load listener.
function load(url) {
  win?.loadURL(url).catch(() => {})
}

function showOffline() {
  failures += 1
  const delay = Math.min(30, 2 ** Math.min(failures, 5))
  win
    .loadFile(path.join(__dirname, 'offline.html'), {
      query: { target: appUrl.href, host: appUrl.host, delay: String(delay), lang },
    })
    .catch(() => {})
}

function showWindow() {
  if (!win) {
    openSetup()
    return
  }
  if (win.isMinimized()) win.restore()
  win.show()
  win.focus()
  refreshTray()
}

function toggleWindow() {
  if (win?.isVisible() && !setupWin) {
    win.hide()
    refreshTray()
  } else {
    showWindow()
  }
}

function createWindow(startHidden) {
  const saved = readState()
  win = new BrowserWindow({
    ...visibleBounds(saved.bounds),
    width: saved.bounds?.width ?? 1100,
    height: saved.bounds?.height ?? 780,
    minWidth: 360,
    minHeight: 480,
    show: false,
    autoHideMenuBar: true,
    backgroundColor: BACKGROUND,
    icon: path.join(ICONS, '512x512.png'),
    title: 'Note',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'preload.js'),
      backgroundThrottling: false,
      autoplayPolicy: 'no-user-gesture-required',
      devTools: dev,
    },
  })
  if (saved.maximized) win.maximize()
  if (saved.fullscreen) win.setFullScreen(true)

  win.once('ready-to-show', () => {
    if (!startHidden) win.show()
  })

  let saveTimer = null
  const saveBounds = () => {
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => {
      if (!win || win.isDestroyed() || win.isFullScreen()) return
      const maximized = win.isMaximized()
      writeState(maximized ? { maximized } : { maximized, bounds: win.getBounds() })
    }, 400)
  }
  win.on('resize', saveBounds)
  win.on('move', saveBounds)
  win.on('enter-full-screen', () => writeState({ fullscreen: true }))
  win.on('leave-full-screen', () => writeState({ fullscreen: false }))

  win.on('close', (event) => {
    if (quitting) return
    event.preventDefault()
    win.hide()
    refreshTray()
    hintTray()
  })
  win.on('focus', () => win.flashFrame(false))
  const pageGone = () => {
    holdCall(false)
    closeRing()
    openReady = false
  }
  win.webContents.on('did-start-loading', pageGone)
  win.webContents.on('render-process-gone', pageGone)
  const tellShown = () => {
    refreshTray()
    if (!win.isDestroyed()) win.webContents.send('desktop:shown', win.isVisible())
  }
  win.on('show', tellShown)
  win.on('hide', tellShown)

  const { webContents } = win
  webContents.setWindowOpenHandler(({ url }) => {
    if (sameOrigin(url)) load(url)
    else openExternal(url)
    return { action: 'deny' }
  })
  webContents.on('will-navigate', (event, url) => {
    if (sameOrigin(url)) return
    event.preventDefault()
    openExternal(url)
  })
  webContents.on('did-fail-load', (_e, code, _desc, url, isMainFrame) => {
    if (!isMainFrame || code === -3 || !sameOrigin(url)) return
    showOffline()
  })
  webContents.on('did-finish-load', () => {
    if (!sameOrigin(webContents.getURL())) return
    failures = 0
    webContents.setZoomLevel(clampZoom(readState().zoom))
    webContents
      .executeJavaScript('document.documentElement.lang')
      .then((tag) => {
        const next = pickLang(tag)
        if (next === lang) return
        lang = next
        writeState({ lang })
        Menu.setApplicationMenu(appMenu())
        refreshTray()
      })
      .catch(() => {})
  })
  webContents.session.setPermissionRequestHandler((wc, permission, callback, details) => {
    if (!sameOrigin(details.requestingUrl || wc.getURL())) return callback(false)
    if (permission === 'media' && process.platform === 'darwin') {
      systemPreferences.askForMediaAccess('microphone').then(callback, () => callback(false))
      return
    }
    callback(true)
  })
  webContents.session.setPermissionCheckHandler((wc, _permission, requestingOrigin) =>
    sameOrigin(requestingOrigin || wc?.getURL() || ''))
  webContents.on('zoom-changed', (_e, direction) => zoomBy(direction === 'in' ? ZOOM_STEP : -ZOOM_STEP))
  webContents.on('context-menu', (_e, params) => contextMenu(params))
  if (process.platform !== 'darwin') {
    const langs = spellLanguages(webContents.session.availableSpellCheckerLanguages, ['en-US', 'ja'])
    if (langs.length) webContents.session.setSpellCheckerLanguages(langs)
  }

  load(appUrl.href)
}

const ZOOM_STEP = 0.5

function setZoom(level) {
  if (!win || !sameOrigin(win.webContents.getURL())) return
  const next = clampZoom(level)
  win.webContents.setZoomLevel(next)
  writeState({ zoom: next })
}

const zoomBy = (step) => win && setZoom(win.webContents.getZoomLevel() + step)

function contextMenu(params) {
  const { webContents } = win
  const items = []
  const gap = () => items.length && items.at(-1).type !== 'separator' && items.push({ type: 'separator' })
  if (params.misspelledWord) {
    for (const word of params.dictionarySuggestions.slice(0, 5)) {
      items.push({ label: word, click: () => webContents.replaceMisspelling(word) })
    }
    items.push({
      label: s('addToDictionary'),
      click: () => webContents.session.addWordToSpellCheckerDictionary(params.misspelledWord),
    })
    gap()
  }
  if (/^https?:/.test(params.linkURL)) {
    items.push(
      { label: s('openLink'), click: () => openExternal(params.linkURL) },
      { label: s('copyLink'), click: () => clipboard.writeText(params.linkURL) },
    )
    gap()
  }
  const can = params.editFlags
  if (params.isEditable) {
    items.push(
      { ...role('cut'), enabled: can.canCut },
      { ...role('copy'), enabled: can.canCopy },
      { ...role('paste'), enabled: can.canPaste },
      { type: 'separator' },
      { ...role('selectAll'), enabled: can.canSelectAll },
    )
  } else if (params.selectionText.trim()) {
    items.push(role('copy'))
  }
  if (items.at(-1)?.type === 'separator') items.pop()
  if (items.length) Menu.buildFromTemplate(items).popup({ window: win })
}

// Closing to the tray says so once, where a closed window usually means a quit app.
function hintTray() {
  if (process.platform === 'darwin' || readState().trayHinted || !Notification.isSupported()) return
  writeState({ trayHinted: true })
  new Notification({ title: 'Note', body: s('trayHint'), silent: true }).show()
}

// A path is handed over once the page says it listens; until then the latest one waits.
function openPath(appPath) {
  if (openReady && win && sameOrigin(win.webContents.getURL())) win.webContents.send('desktop:open', appPath)
  else pendingOpen = appPath
}

const RING_SIZE = { width: 320, height: 96 }
const RING_MARGIN = 12
// Past the page's own ring timeout, in case the page never says the ring stopped.
const RING_LIMIT_MS = 45_000

function activeArea() {
  try {
    return screen.getDisplayNearestPoint(screen.getCursorScreenPoint()).workArea
  } catch {
    return screen.getPrimaryDisplay().workArea
  }
}

// The ring floats in a corner without taking focus or raising the main window.
function openRing(info) {
  ringShown = info
  clearTimeout(ringLimit)
  ringLimit = setTimeout(closeRing, RING_LIMIT_MS)
  if (ringWin) {
    ringWin.webContents.send('ring:info', info)
    return
  }
  const popup = new BrowserWindow({
    ...cornerBounds(activeArea(), RING_SIZE, RING_MARGIN),
    show: false,
    frame: false,
    transparent: true,
    hasShadow: false,
    resizable: false,
    maximizable: false,
    minimizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    alwaysOnTop: true,
    title: 'Note',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'ring-preload.js'),
      devTools: dev,
    },
  })
  ringWin = popup
  popup.setAlwaysOnTop(true, 'pop-up-menu')
  popup.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true })
  popup.on('closed', () => {
    if (ringWin === popup) ringWin = null
  })
  popup.webContents.on('will-navigate', (event) => event.preventDefault())
  popup.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  popup.webContents.on('did-finish-load', () => popup.webContents.send('ring:info', ringShown))
  popup.once('ready-to-show', () => {
    if (ringWin !== popup) return
    popup.showInactive()
    popup.flashFrame(true)
    if (process.platform === 'darwin') ringBounce = app.dock?.bounce('critical') ?? null
  })
  popup.loadFile(path.join(__dirname, 'ring.html'), { query: { lang } }).catch(() => {})
}

// Forgotten at once, so a ring right after this opens a fresh popup rather than the one closing.
function closeRing() {
  clearTimeout(ringLimit)
  ringLimit = null
  if (ringBounce !== null) app.dock?.cancelBounce(ringBounce)
  ringBounce = null
  const popup = ringWin
  ringWin = null
  if (popup && !popup.isDestroyed()) popup.close()
}

const LAUNCHER_WIDTH = 640

// macOS and Windows draw the frosted glass natively; elsewhere the page paints its own over a transparent window.
function launcherGlass() {
  if (process.platform === 'darwin') return { transparent: true, vibrancy: 'popover', visualEffectState: 'active', roundedCorners: true }
  if (process.platform === 'win32') return { backgroundColor: '#00000000', backgroundMaterial: 'acrylic', roundedCorners: true }
  return { transparent: true, hasShadow: false }
}

function placeLauncher() {
  const { x, y } = centerTop(activeArea(), { width: LAUNCHER_WIDTH, height: LAUNCHER_HEIGHT.min })
  launcherWin.setBounds({ x, y, width: LAUNCHER_WIDTH, height: launcherWin.getBounds().height })
}

function showLauncher() {
  placeLauncher()
  launcherWin.show()
  launcherWin.focus()
  launcherWin.webContents.send('launcher:shown')
}

function hideLauncher() {
  if (!launcherWin?.isVisible()) return
  launcherWin.hide()
  if (process.platform === 'darwin' && !BrowserWindow.getAllWindows().some((w) => w.isVisible())) app.hide()
}

// A one-line ask from anywhere: the server's own page in a launcher mode, sharing the main window's sign-in.
function toggleLauncher() {
  if (!appUrl) return showWindow()
  if (launcherWin?.isVisible()) return hideLauncher()
  if (launcherWin) return showLauncher()
  const glass = launcherGlass()
  launcherWin = new BrowserWindow({
    ...centerTop(activeArea(), { width: LAUNCHER_WIDTH, height: LAUNCHER_HEIGHT.min }),
    show: false,
    frame: false,
    resizable: false,
    maximizable: false,
    minimizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    alwaysOnTop: true,
    title: 'Note',
    ...glass,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'launcher-preload.js'),
      devTools: dev,
    },
  })
  launcherWin.setAlwaysOnTop(true, 'pop-up-menu')
  launcherWin.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true })
  launcherWin.on('blur', hideLauncher)
  launcherWin.on('closed', () => {
    launcherWin = null
  })
  const { webContents } = launcherWin
  webContents.on('will-navigate', (event) => event.preventDefault())
  webContents.setWindowOpenHandler(({ url }) => {
    if (!sameOrigin(url)) openExternal(url)
    return { action: 'deny' }
  })
  launcherWin.once('ready-to-show', showLauncher)
  const page = new URL(appUrl.href)
  page.searchParams.set('launcher', glass.vibrancy || glass.backgroundMaterial ? 'glass' : 'plain')
  launcherWin.loadURL(page.href).catch(() => {})
}

function closeLauncher() {
  if (launcherWin && !launcherWin.isDestroyed()) launcherWin.close()
}

const fromLauncher = (event) =>
  Boolean(launcherWin) && event.sender === launcherWin.webContents && sameOrigin(event.senderFrame?.url ?? '')

ipcMain.on('launcher:hide', (event) => {
  if (fromLauncher(event)) hideLauncher()
})

ipcMain.on('launcher:resize', (event, height) => {
  if (!fromLauncher(event)) return
  const bounds = launcherWin.getBounds()
  launcherWin.setBounds({ ...bounds, height: launcherHeight(height) })
})

// The thread a launcher answer belongs to opens in the main window.
ipcMain.on('launcher:open', (event, conversationId) => {
  if (!fromLauncher(event)) return
  hideLauncher()
  showWindow()
  if (Number.isSafeInteger(conversationId) && conversationId > 0) openPath(`chat/${conversationId}`)
})

const WIDGET_SIZE = { width: 180, height: 64 }

function widgetPosition() {
  const saved = readState().widget
  if (saved && visibleBounds({ ...saved, ...WIDGET_SIZE })?.x !== undefined) return saved
  const area = screen.getPrimaryDisplay().workArea
  return { x: area.x + area.width - WIDGET_SIZE.width - 24, y: area.y + area.height - WIDGET_SIZE.height - 24 }
}

// The session clock floating over everything else, open only while a session runs.
function openWidget() {
  if (widgetWin) return
  widgetWin = new BrowserWindow({
    ...WIDGET_SIZE,
    ...widgetPosition(),
    show: false,
    frame: false,
    transparent: true,
    hasShadow: false,
    resizable: false,
    maximizable: false,
    minimizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    alwaysOnTop: true,
    title: 'Note',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'widget-preload.js'),
      devTools: dev,
    },
  })
  widgetWin.setAlwaysOnTop(true, 'floating')
  widgetWin.setVisibleOnAllWorkspaces(true)
  widgetWin.on('moved', () => {
    if (!widgetWin || widgetWin.isDestroyed()) return
    const { x, y } = widgetWin.getBounds()
    writeState({ widget: { x, y } })
  })
  widgetWin.on('closed', () => {
    widgetWin = null
  })
  widgetWin.webContents.on('will-navigate', (event) => event.preventDefault())
  widgetWin.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  widgetWin.webContents.on('did-finish-load', () => widgetWin?.webContents.send('widget:clock', widgetClock))
  widgetWin.once('ready-to-show', () => widgetWin?.showInactive())
  widgetWin.loadFile(path.join(__dirname, 'widget.html')).catch(() => {})
}

function closeWidget() {
  if (widgetWin && !widgetWin.isDestroyed()) widgetWin.close()
}

function openSetup() {
  if (setupWin) {
    setupWin.show()
    setupWin.focus()
    return
  }
  const parent = win?.isVisible() ? win : undefined
  setupWin = new BrowserWindow({
    width: 420,
    height: 220,
    resizable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    show: false,
    parent,
    modal: Boolean(parent),
    backgroundColor: BACKGROUND,
    icon: path.join(ICONS, '512x512.png'),
    title: 'Note',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'setup-preload.js'),
      devTools: dev,
    },
  })
  setupWin.setMenu(null)
  setupWin.once('ready-to-show', () => setupWin.show())
  setupWin.on('closed', () => {
    setupWin = null
    if (!appUrl) app.quit()
  })
  setupWin.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  setupWin.webContents.on('will-navigate', (event) => event.preventDefault())
  setupWin.loadFile(path.join(__dirname, 'setup.html'), { query: { lang, current: appUrl?.href ?? '' } }).catch(() => {})
}

// Any answer from /api/me (signed out is a 401) or a successful root counts as a Note server.
async function reachable(base) {
  for (const [route, accept] of [
    ['api/me', (res) => res.ok || res.status === 401],
    ['', (res) => res.ok],
  ]) {
    try {
      const res = await net.fetch(new URL(route, base).href, { signal: AbortSignal.timeout(8000), cache: 'no-store' })
      if (accept(res)) return true
    } catch {}
  }
  return false
}

function useServer(url) {
  patchJson(configPath(), { serverUrl: url })
  server = { url, source: 'saved' }
  appUrl = new URL(url)
  if (process.platform === 'linux' && autostartEnabled()) setAutostart(true)
  setupWin?.close()
  closeLauncher()
  if (win) {
    failures = 0
    load(appUrl.href)
    showWindow()
  } else {
    createWindow(false)
  }
  refreshTray()
}

ipcMain.handle('setup:connect', async (event, address) => {
  if (!setupWin || event.sender !== setupWin.webContents) return { ok: false, reason: 'unreachable' }
  const url = normalizeServerUrl(address)
  if (!url) return { ok: false, reason: 'invalid' }
  if (!(await reachable(url))) return { ok: false, reason: 'unreachable' }
  useServer(url)
  return { ok: true }
})

// Only the main window's own page on the server may drive the shell.
const fromApp = (event) => Boolean(win) && event.sender === win.webContents && sameOrigin(event.senderFrame?.url ?? '')

// `nightly` is null where the app does not update itself.
const shellSettings = () => ({
  startAtLogin: autostartEnabled(),
  nightly: updates === 'self' ? nightlyOn() : null,
  launcher: shortcutLabel(),
})

ipcMain.handle('desktop:about', (event) => (fromApp(event) ? { version: app.getVersion(), server: appUrl?.host ?? null } : null))

ipcMain.on('desktop:show', (event) => {
  if (fromApp(event)) showWindow()
})

ipcMain.handle('desktop:settings', (event) => (fromApp(event) ? shellSettings() : null))

ipcMain.handle('desktop:shown', (event) => (fromApp(event) ? win.isVisible() : null))

ipcMain.handle('desktop:set', (event, key, on) => {
  if (!fromApp(event) || typeof on !== 'boolean') return null
  if (key === 'startAtLogin') {
    setAutostart(on)
    refreshTray()
  } else if (key === 'nightly' && updates === 'self') setNightly(on)
  return shellSettings()
})

// While a call is open the screen stays awake and other players are paused,
// then resumed once it ends.
function holdCall(on) {
  if (on && !callHold) {
    callHold = { blocker: powerSaveBlocker.start('prevent-display-sleep'), resume: pauseOthers() }
  } else if (!on && callHold) {
    const { blocker, resume } = callHold
    callHold = null
    powerSaveBlocker.stop(blocker)
    resume.then((play) => play()).catch(() => {})
  }
}

ipcMain.on('desktop:inCall', (event, on) => {
  if (fromApp(event)) holdCall(on === true)
})

ipcMain.on('desktop:widget', (event) => {
  if (!fromApp(event)) return
  if (widgetWin) closeWidget()
  else if (widgetClock) openWidget()
})

// The clock is plain numbers and flags; null means the session ended, which closes the widget.
ipcMain.on('desktop:widgetClock', (event, clock) => {
  if (!fromApp(event)) return
  widgetClock = clock && typeof clock === 'object' ? clock : null
  if (!widgetClock) closeWidget()
  else widgetWin?.webContents.send('widget:clock', widgetClock)
})

const fromWidget = (event) => Boolean(widgetWin) && event.sender === widgetWin.webContents

ipcMain.on('widget:close', (event) => {
  if (fromWidget(event)) closeWidget()
})

ipcMain.on('widget:show', (event) => {
  if (fromWidget(event)) showWindow()
})

ipcMain.on('desktop:changeServer', (event) => {
  if (fromApp(event)) openSetup()
})

// A page from before the popup sends no details and expects the window raised.
ipcMain.on('desktop:ring', (event, raw) => {
  if (!fromApp(event)) return
  const info = ringInfo(raw)
  if (info) return openRing(info)
  showWindow()
  if (process.platform === 'darwin') app.dock?.bounce('critical')
  else win.flashFrame(true)
})

ipcMain.on('desktop:ringStop', (event) => {
  if (fromApp(event)) closeRing()
})

const fromRing = (event) => Boolean(ringWin) && event.sender === ringWin.webContents

ipcMain.on('ring:answer', (event) => {
  if (!fromRing(event)) return
  closeRing()
  showWindow()
  win?.webContents.send('desktop:ringAction', 'answer')
})

ipcMain.on('ring:decline', (event) => {
  if (!fromRing(event)) return
  closeRing()
  win?.webContents.send('desktop:ringAction', 'decline')
})

// `status` is the OS's own answer where it keeps one (macOS, Windows), else 'unknown'.
function micAccess() {
  let status = 'unknown'
  if (process.platform === 'darwin' || process.platform === 'win32') {
    try {
      status = systemPreferences.getMediaAccessStatus('microphone')
    } catch {}
  }
  return { status, settings: micSettingsUrl(process.platform) !== null }
}

ipcMain.handle('desktop:micAccess', (event) => (fromApp(event) ? micAccess() : null))

ipcMain.on('desktop:micSettings', (event) => {
  const url = micSettingsUrl(process.platform)
  if (fromApp(event) && url) shell.openExternal(url).catch(() => {})
})

ipcMain.on('desktop:openReady', (event) => {
  if (!fromApp(event)) return
  openReady = true
  if (pendingOpen) {
    const waiting = pendingOpen
    pendingOpen = null
    openPath(waiting)
  }
})

// Waking up or getting the network back: the offline page retries now, a loaded page reconnects on its own terms.
function resumed() {
  if (!win || !appUrl) return
  if (sameOrigin(win.webContents.getURL())) {
    win.webContents.send('desktop:resume')
  } else {
    failures = 0
    load(appUrl.href)
  }
}

const updates = updateMode({ platform: process.platform, packaged: app.isPackaged, dev, env: process.env })
const nightlyOn = () => readJson(configPath()).nightly === true

function startUpdates() {
  if (updates !== 'self') return
  try {
    updater = require('electron-updater').autoUpdater
  } catch {
    return
  }
  updater.autoDownload = true
  updater.autoInstallOnAppQuit = true
  updater.allowPrerelease = nightlyOn()
  updater.on('error', () => {})
  updater.on('update-downloaded', () => {
    updateReady = true
    Menu.setApplicationMenu(appMenu())
    refreshTray()
  })
  checkUpdates()
  setInterval(checkUpdates, CHECK_EVERY_MS)
}

function checkUpdates() {
  if (updates === 'page') shell.openExternal(RELEASES_URL)
  else updater?.checkForUpdates().catch(() => {})
}

function setNightly(on) {
  patchJson(configPath(), { nightly: on })
  if (updater) updater.allowPrerelease = on
  checkUpdates()
  Menu.setApplicationMenu(appMenu())
  refreshTray()
}

function installUpdate() {
  quitting = true
  updater?.quitAndInstall()
}

// Shared by the tray and the File menu; empty where this build does not update itself or point anywhere.
function updateItems() {
  if (!updates) return []
  return [
    ...(updateReady ? [{ label: s('restartToUpdate'), click: installUpdate }] : []),
    { label: s('checkUpdates'), click: checkUpdates },
    ...(updates === 'self'
      ? [{ label: s('nightly'), type: 'checkbox', checked: nightlyOn(), click: (item) => setNightly(item.checked) }]
      : []),
    { type: 'separator' },
  ]
}

function refreshTray() {
  if (!tray) return
  const visible = win?.isVisible()
  tray.setContextMenu(
    Menu.buildFromTemplate([
      {
        label: visible ? s('hide') : s('show'),
        click: () => (visible ? win.hide() : showWindow()),
      },
      { label: s('changeServer'), click: openSetup },
      {
        label: s('startAtLogin'),
        type: 'checkbox',
        checked: autostartEnabled(),
        click: (item) => {
          setAutostart(item.checked)
          refreshTray()
        },
      },
      { label: s('reload'), enabled: Boolean(appUrl), click: () => load(appUrl.href) },
      { type: 'separator' },
      ...updateItems(),
      { label: s('quit'), click: () => app.quit() },
    ]),
  )
}

function createTray() {
  const image = nativeImage.createFromPath(path.join(ICONS, 'tray.png'))
  tray = new Tray(image)
  tray.setToolTip('Note')
  tray.on('click', toggleWindow)
  refreshTray()
}

function appMenu() {
  const zoom = (name, accelerator, click, extra = {}) => ({ label: s(name), accelerator, click, ...extra })
  const view = [
    { ...role('reload'), label: s('reload') },
    { ...role('forceReload'), label: s('forceReload') },
    ...(dev ? [role('toggleDevTools')] : []),
    { type: 'separator' },
    zoom('resetZoom', 'CommandOrControl+0', () => setZoom(0)),
    zoom('zoomIn', 'CommandOrControl+Plus', () => zoomBy(ZOOM_STEP)),
    zoom('zoomIn', 'CommandOrControl+=', () => zoomBy(ZOOM_STEP), { visible: false }),
    zoom('zoomOut', 'CommandOrControl+-', () => zoomBy(-ZOOM_STEP)),
    { type: 'separator' },
    role('togglefullscreen'),
  ]
  const edit = ['undo', 'redo', null, 'cut', 'copy', 'paste', 'selectAll'].map((r) =>
    r ? role(r) : { type: 'separator' },
  )
  return Menu.buildFromTemplate([
    ...(process.platform === 'darwin' ? [{ role: 'appMenu' }] : []),
    {
      label: s('file'),
      submenu: [
        {
          label: s('settings'),
          accelerator: 'CommandOrControl+,',
          enabled: Boolean(appUrl),
          click: () => {
            showWindow()
            openPath('settings')
          },
        },
        { label: s('changeServer'), click: openSetup },
        { type: 'separator' },
        ...updateItems(),
        { role: 'close', label: s('close') },
        { label: s('quit'), accelerator: 'CommandOrControl+Q', click: () => app.quit() },
      ],
    },
    { label: s('edit'), submenu: edit },
    { label: s('view'), submenu: view },
    { label: s('window'), submenu: [role('minimize'), ...(process.platform === 'darwin' ? [role('zoom')] : []), { role: 'close', label: s('close') }] },
  ])
}

const LAUNCHER_SHORTCUT = 'CommandOrControl+Shift+Space'
let shortcutTaken = false

function registerShortcut() {
  try {
    return globalShortcut.register(LAUNCHER_SHORTCUT, toggleLauncher)
  } catch {
    return false
  }
}

const shortcutLabel = () =>
  shortcutTaken ? null : process.platform === 'darwin' ? '⌘⇧Space' : 'Ctrl+Shift+Space'

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  // Windows shows an app's notifications only under the id its installer gave the shortcut.
  if (process.platform === 'win32') app.setAppUserModelId('net.shuntia.note')
  app.on('second-instance', (_e, argv) => {
    if (!argv.includes('--hidden')) showWindow()
  })
  app.on('before-quit', () => {
    quitting = true
    holdCall(false)
    closeRing()
    closeLauncher()
  })
  app.on('will-quit', () => globalShortcut.unregisterAll())
  app.on('activate', showWindow)
  app.on('window-all-closed', () => {})

  app.whenReady().then(() => {
    const startHidden =
      process.argv.includes('--hidden') ||
      (process.platform === 'darwin' && app.getLoginItemSettings().wasOpenedAtLogin)
    lang = pickLang(readState().lang || app.getLocale())
    Menu.setApplicationMenu(appMenu())
    server = resolveServerUrl({
      argv: process.argv,
      env: process.env,
      saved: readJson(configPath()).serverUrl,
      baked: BAKED_URL,
    })
    if (server) appUrl = new URL(server.url)
    initAutostart()
    shortcutTaken = !registerShortcut()
    powerMonitor.on('resume', resumed)
    createTray()
    startUpdates()
    if (appUrl) {
      createWindow(startHidden)
    } else if (!startHidden) {
      openSetup()
    }
  })
}
