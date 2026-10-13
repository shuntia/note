import { useSyncExternalStore } from 'react'

// `windowMs` is the undo window the action holds open; the toast must outlast it.
export type ToastAction = { label: string; run: () => void; windowMs?: number }

type Toast = { msg: string; action?: ToastAction; leaving?: boolean }

const TOAST_LEAVE_MS = 200

let current: Toast | null = null
let timer = 0
const listeners = new Set<() => void>()

function put(next: Toast | null) {
  current = next
  for (const listen of listeners) listen()
}

function subscribe(listen: () => void) {
  listeners.add(listen)
  return () => void listeners.delete(listen)
}

export function dismissToast() {
  window.clearTimeout(timer)
  put(current && { ...current, leaving: true })
  timer = window.setTimeout(() => put(null), TOAST_LEAVE_MS)
}

export function notify(msg: string, action?: ToastAction) {
  put({ msg, action })
  window.clearTimeout(timer)
  timer = window.setTimeout(dismissToast, action ? (action.windowMs ?? 5000) : 4000)
}

// Onboarding draws the message alone, without `actions`.
export function Toaster({ aboveTabs = false, actions = true }: { aboveTabs?: boolean; actions?: boolean }) {
  const toast = useSyncExternalStore(subscribe, () => current)
  if (!toast) return null
  return (
    <div className={`toast${aboveTabs ? ' above-tabs' : ''}${toast.leaving ? ' leaving' : ''}`} role="status">
      <span className="toast-msg">{toast.msg}</span>
      {actions && toast.action && (
        <button
          className="toast-action"
          onClick={() => {
            toast.action?.run()
            dismissToast()
          }}
        >
          {toast.action.label}
        </button>
      )}
    </div>
  )
}
