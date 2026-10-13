import { lazy, StrictMode, Suspense } from 'react'
import { createRoot } from 'react-dom/client'
import { askMeEarly } from './early'
import { bootLocale } from './i18n'
import './styles.css'
import { applyTheme, storedTheme } from './theme'

const App = lazy(() => import('./app').then((m) => ({ default: m.App })))
const JoinPage = lazy(() => import('./views/Join').then((m) => ({ default: m.JoinPage })))
const SharePage = lazy(() => import('./views/Share').then((m) => ({ default: m.SharePage })))

applyTheme(storedTheme())
bootLocale()

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js').catch(() => {})
}

const shareToken = /^\/s\/([A-Za-z0-9_-]+)\/?$/.exec(location.pathname)?.[1] ?? null
const joinToken = /^\/join\/([A-Za-z0-9_-]+)\/?$/.exec(location.pathname)?.[1] ?? null

if (!shareToken && !joinToken) askMeEarly()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Suspense fallback={null}>
      {shareToken ? <SharePage token={shareToken} /> : joinToken ? <JoinPage token={joinToken} /> : <App />}
    </Suspense>
  </StrictMode>,
)
