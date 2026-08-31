import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './app'
import './styles.css'
import { applyTheme, storedTheme } from './theme'

applyTheme(storedTheme())

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js').catch(() => {})
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
