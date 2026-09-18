import { useEffect, useState } from 'react'
import { api, ApiError } from './api'
import type { Review } from './types'

const FOLD_KEY = 'note.reviewFolded'

function readFold(week: string): boolean {
  try {
    const raw = localStorage.getItem(FOLD_KEY)
    if (!raw) return true
    const saved = JSON.parse(raw) as { week?: string; folded?: boolean }
    return saved.week === week ? saved.folded !== false : true
  } catch {
    return true
  }
}

function writeFold(week: string, folded: boolean) {
  try {
    localStorage.setItem(FOLD_KEY, JSON.stringify({ week, folded }))
  } catch {
    // storage blocked; the fold still holds for this session
  }
}

function mondayOf(day: Date): string {
  const at = new Date(day)
  at.setHours(0, 0, 0, 0)
  at.setDate(at.getDate() - ((at.getDay() + 6) % 7))
  const month = `${at.getMonth() + 1}`.padStart(2, '0')
  return `${at.getFullYear()}-${month}-${`${at.getDate()}`.padStart(2, '0')}`
}

// A letter older than last week belongs to a week nobody is still living in.
function stillCurrent(week: string): boolean {
  const today = new Date()
  const previous = new Date(today)
  previous.setDate(previous.getDate() - 7)
  return week === mondayOf(today) || week === mondayOf(previous)
}

function weekWords(week: string): string {
  const monday = new Date(`${week}T00:00`)
  if (Number.isNaN(monday.getTime())) return week
  const sunday = new Date(monday)
  sunday.setDate(sunday.getDate() + 6)
  const day = (at: Date) => at.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
  return `${day(monday)} – ${day(sunday)}`
}

export function ReviewFold() {
  const [review, setReview] = useState<Review | null | undefined>(undefined)
  const [folded, setFolded] = useState(true)

  useEffect(() => {
    api
      .review()
      .then((r) => {
        if (!stillCurrent(r.week_start)) {
          setReview(null)
          return
        }
        setReview(r)
        setFolded(readFold(r.week_start))
      })
      .catch((err) => {
        if (!(err instanceof ApiError)) throw err
        setReview(null)
      })
  }, [])

  if (!review) return null

  const toggle = () => {
    const next = !folded
    setFolded(next)
    writeFold(review.week_start, next)
  }

  return (
    <section className={`debrief-row review-row${folded ? '' : ' open'}`}>
      <button className="debrief-fold" aria-expanded={!folded} onClick={toggle}>
        <span className="debrief-mark" aria-hidden="true" />
        <span className="debrief-lead">
          <b>Your week:</b> {weekWords(review.week_start)}
        </span>
        <span className="debrief-chev" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </span>
      </button>
      {!folded && <div className="letter">{review.content}</div>}
    </section>
  )
}
