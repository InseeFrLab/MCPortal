// Shared easing and entrance animation, so every block fades in the same way.
export const EASE = [0.22, 1, 0.36, 1] as const

export function fadeUp(reducedMotion: boolean | null, delay: number, distance = 12) {
  if (reducedMotion) return {}
  return {
    initial: { opacity: 0, y: distance } as const,
    animate: { opacity: 1, y: 0 } as const,
    transition: { duration: 0.7, delay, ease: EASE },
  }
}
