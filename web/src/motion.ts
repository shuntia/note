export const REDUCED_MOTION = '(prefers-reduced-motion: reduce)'
export const reducedMotion = () => window.matchMedia(REDUCED_MOTION).matches
