const SWIPE_PX = 80

// A downward drag on the call view; the click its own release fires is not a tap, and nothing later is swallowed.
export class Swipe {
  private from: number | null = null
  private swallow = false

  down(y: number) {
    this.from = y
    this.swallow = false
  }

  // True when the release finishes a swipe.
  up(y: number): boolean {
    const from = this.from
    this.from = null
    this.swallow = from !== null && y - from > SWIPE_PX
    return this.swallow
  }

  // Once the release's own click has had its turn.
  settle() {
    this.from = null
    this.swallow = false
  }

  // True when a click on the circle is a tap.
  tap(): boolean {
    const tap = !this.swallow
    this.swallow = false
    return tap
  }
}
