import { t } from '../i18n'

// The call takes focus off the field first, so focus coming back after the call raises no keyboard.
export function pressCall(button: { focus(): void }, onCall: () => void) {
  button.focus()
  onCall()
}

// Send while there is text, a call to Note while there is none; a send keeps the field focused so the tab bar stays down.
export function ComposeButton({
  draft,
  busy,
  micBlocked,
  onCall,
}: {
  draft: string
  busy: boolean
  micBlocked: boolean
  onCall?: () => void
}) {
  const send = !!draft.trim() || !onCall
  return (
    <button
      type={send ? 'submit' : 'button'}
      className="compose-button"
      data-face={send ? 'send' : 'call'}
      aria-label={send ? t('talk.send') : micBlocked ? t('talk.micBlocked') : t('talk.call')}
      disabled={send ? busy || !draft.trim() : busy || micBlocked}
      onMouseDown={(e) => e.preventDefault()}
      onClick={send || !onCall ? undefined : (e) => pressCall(e.currentTarget, onCall)}
    >
      <svg className="face-send" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M5 12h14" />
        <path d="M13 6l6 6-6 6" />
      </svg>
      {onCall && (
        <svg className="face-call" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 10.5v3M8 7.5v9M12 4v16M16 8.5v7M20 10.5v3" />
          {micBlocked && <path d="M4 4l16 16" />}
        </svg>
      )}
    </button>
  )
}
