'use strict'

const { app, BrowserWindow, Menu, Tray, nativeImage, screen, shell } = require('electron')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')

const DEFAULT_URL = require('./package.json').noteUrl || 'http://127.0.0.1:3271'
const ICONS = path.join(__dirname, 'icons')

function argValue(name) {
  const prefix = `--${name}=`
  const hit = process.argv.find((a) => a.startsWith(prefix))
  return hit ? hit.slice(prefix.length) : undefined
}

const appUrl = new URL(argValue('url') || process.env.NOTE_URL || DEFAULT_URL)
const dev = process.argv.includes('--dev')

let win = null
let tray = null
let quitting = false
let failures = 0

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

function readState() {
  try {
    return JSON.parse(fs.readFileSync(statePath(), 'utf8'))
  } catch {
    return {}
  }
}

function writeState(patch) {
  const next = { ...readState(), ...patch }
  try {
    fs.mkdirSync(path.dirname(statePath()), { recursive: true })
    fs.writeFileSync(statePath(), JSON.stringify(next))
  } catch {}
}

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
  const extra = appUrl.href === new URL(DEFAULT_URL).href ? [] : [`--url=${appUrl.href}`]
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
  if (!win) return
  if (win.isMinimized()) win.restore()
  win.show()
  win.focus()
  refreshTray()
}

function toggleWindow() {
  if (win?.isVisible()) {
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
    backgroundColor: '#f0e8de',
    icon: path.join(ICONS, '512x512.png'),
    title: 'Note',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      devTools: dev,
    },
  })
  if (saved.maximized) win.maximize()

  win.once('ready-to-show', () => {
    if (!startHidden) win.show()
  })

  let saveTimer = null
  const saveBounds = () => {
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => {
      if (!win || win.isDestroyed()) return
      const maximized = win.isMaximized()
      writeState(maximized ? { maximized } : { maximized, bounds: win.getBounds() })
    }, 400)
  }
  win.on('resize', saveBounds)
  win.on('move', saveBounds)

  win.on('close', (event) => {
    if (quitting) return
    event.preventDefault()
    win.hide()
    refreshTray()
  })
  win.on('show', refreshTray)
  win.on('hide', refreshTray)

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
  webContents.session.setPermissionRequestHandler((wc, _permission, callback, details) => {
    callback(sameOrigin(details.requestingUrl || wc.getURL()))
  })

  load(appUrl.href)
}

function refreshTray() {
  if (!tray) return
  const visible = win?.isVisible()
  tray.setContextMenu(
    Menu.buildFromTemplate([
      { label: visible ? s('hide') : s('show'), click: () => (visible ? win.hide() : showWindow()) },
      {
        label: s('startAtLogin'),
        type: 'checkbox',
        checked: autostartEnabled(),
        click: (item) => {
          setAutostart(item.checked)
          refreshTray()
        },
      },
      { label: s('reload'), click: () => load(appUrl.href) },
      { type: 'separator' },
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
  const view = [
    { ...role('reload'), label: s('reload') },
    { ...role('forceReload'), label: s('forceReload') },
    ...(dev ? [role('toggleDevTools')] : []),
    { type: 'separator' },
    role('resetZoom'),
    role('zoomIn'),
    { ...role('zoomIn'), accelerator: 'CommandOrControl+=', visible: false },
    role('zoomOut'),
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
        { role: 'close', label: s('close') },
        { label: s('quit'), accelerator: 'CommandOrControl+Q', click: () => app.quit() },
      ],
    },
    { label: s('edit'), submenu: edit },
    { label: s('view'), submenu: view },
    { label: s('window'), submenu: [role('minimize'), ...(process.platform === 'darwin' ? [role('zoom')] : []), { role: 'close', label: s('close') }] },
  ])
}

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  app.on('second-instance', (_e, argv) => {
    if (!argv.includes('--hidden')) showWindow()
  })
  app.on('before-quit', () => {
    quitting = true
  })
  app.on('activate', showWindow)
  app.on('window-all-closed', () => {})

  app.whenReady().then(() => {
    const startHidden =
      process.argv.includes('--hidden') ||
      (process.platform === 'darwin' && app.getLoginItemSettings().wasOpenedAtLogin)
    lang = pickLang(readState().lang || app.getLocale())
    Menu.setApplicationMenu(appMenu())
    initAutostart()
    createWindow(startHidden)
    createTray()
  })
}
