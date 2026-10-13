import { useEffect, useMemo, useState } from 'react'
import { t } from '../../i18n'
import { paletteAt, solarAltitude, sunTimes } from '../../sky'
import { currentPlace } from '../../theme'
import { deviceZone } from '../../zone'

// Today's sky from midnight to midnight at half-hour stops, with a mark at now.
export function SkyStrip() {
  const [minute, setMinute] = useState(() => Math.floor(Date.now() / 60_000))
  useEffect(() => {
    const id = window.setInterval(() => setMinute(Math.floor(Date.now() / 60_000)), 60_000)
    return () => window.clearInterval(id)
  }, [])
  const { times, stops, mins } = useMemo(() => {
    const zone = deviceZone()
    const place = currentPlace()
    const now = new Date(minute * 60_000)
    const stops = Array.from({ length: 49 }, (_, i) => {
      const at = new Date(now)
      at.setHours(0, i * 30, 0, 0)
      const { tokens } = paletteAt(solarAltitude(at, place.lat, place.lon))
      return `${tokens['sky-mid']} ${(i / 48) * 100}%`
    })
    return { times: sunTimes(now, place, zone), stops, mins: now.getHours() * 60 + now.getMinutes() }
  }, [minute])
  return (
    <div className="sky-strip-wrap">
      <div className="sky-strip" style={{ background: `linear-gradient(90deg, ${stops.join(', ')})` }}>
        <i style={{ left: `${(mins / 1440) * 100}%` }} />
      </div>
      <div className="sky-ticks">
        <span>0:00</span>
        <span>{times.rise ? t('settings.sky.rise', { time: times.rise }) : t('settings.sky.noSunrise')}</span>
        <span>{times.set ? t('settings.sky.set', { time: times.set }) : t('settings.sky.noSunset')}</span>
        <span>24:00</span>
      </div>
    </div>
  )
}
