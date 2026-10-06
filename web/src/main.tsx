import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './app'
import { bootLocale } from './i18n'
import './styles.css'
import { applyTheme, storedTheme } from './theme'
import { SharePage } from './views/Share'

applyTheme(storedTheme())
bootLocale()

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js').catch(() => {})
}

const shareToken = /^\/s\/([A-Za-z0-9_-]+)\/?$/.exec(location.pathname)?.[1] ?? null

createRoot(document.getElementById('root')!).render(
  <StrictMode>{shareToken ? <SharePage token={shareToken} /> : <App />}</StrictMode>,
)
