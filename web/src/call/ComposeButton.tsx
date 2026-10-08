import { t } from '../i18n'

// The composer's one button: send while there is text, a call to Note while there is none.
// Both faces stay drawn so the button morphs between them rather than swapping. A press keeps the
// field focused: blurring it brings the phone's tab bar back and moves the button out from under the tap.
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
      data-face={send ? 'send' : 'mic'}
      aria-label={send ? t('talk.send') : micBlocked ? t('talk.micBlocked') : t('talk.call')}
      disabled={send ? busy || !draft.trim() : micBlocked}
      onMouseDown={(e) => e.preventDefault()}
      onClick={send ? undefined : onCall}
    >
      <svg className="face-send" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M5 12h14" />
        <path d="M13 6l6 6-6 6" />
      </svg>
      {onCall && (
        <svg className="face-mic" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="9" y="3" width="6" height="11" rx="3" />
          <path d="M5 11a7 7 0 0 0 14 0" />
          <path d="M12 18v3" />
          {micBlocked && <path d="M4 4l16 16" />}
        </svg>
      )}
    </button>
  )
}
