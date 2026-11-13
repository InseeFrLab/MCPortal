import {
  motion,
  useMotionTemplate,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
} from 'motion/react'
import { useEffect } from 'react'

// Test-chamber backdrop: faint wall panels, a blue glow on the left and an orange glow on
// the right (the two portals), plus a soft light that follows the pointer.
export function Background() {
  const reducedMotion = useReducedMotion()
  const pointerX = useMotionValue(0.5)
  const pointerY = useMotionValue(0.5)

  const springX = useSpring(pointerX, { stiffness: 40, damping: 22, mass: 0.8 })
  const springY = useSpring(pointerY, { stiffness: 40, damping: 22, mass: 0.8 })

  const glowLeft = useTransform(springX, (v) => `${v * 100}%`)
  const glowTop = useTransform(springY, (v) => `${v * 100}%`)
  const parallaxX = useTransform(springX, (v) => `${(v - 0.5) * 24}px`)
  const parallaxY = useTransform(springY, (v) => `${(v - 0.5) * 16}px`)

  // The pointer light shifts from blue to orange as it crosses the page.
  const tint = useTransform(springX, (v) => {
    const r = Math.round(31 + (255 - 31) * v)
    const g = Math.round(163 + (138 - 163) * v)
    const b = Math.round(255 + (31 - 255) * v)
    return `rgba(${r}, ${g}, ${b}, 0.07)`
  })
  const pointerGlow = useMotionTemplate`radial-gradient(560px circle at ${glowLeft} ${glowTop}, ${tint}, transparent 65%)`

  useEffect(() => {
    if (reducedMotion) return
    const onPointerMove = (event: PointerEvent) => {
      pointerX.set(event.clientX / window.innerWidth)
      pointerY.set(event.clientY / window.innerHeight)
    }
    window.addEventListener('pointermove', onPointerMove, { passive: true })
    return () => window.removeEventListener('pointermove', onPointerMove)
  }, [pointerX, pointerY, reducedMotion])

  return (
    <div className="pointer-events-none fixed inset-0 overflow-hidden" aria-hidden>
      <div className="absolute inset-0 bg-lab-black" />
      {/* Wall panels, faded out towards the edges. */}
      <div
        className="absolute inset-0 opacity-[0.35]"
        style={{
          backgroundImage:
            'linear-gradient(rgba(238,242,246,0.045) 1px, transparent 1px), linear-gradient(90deg, rgba(238,242,246,0.045) 1px, transparent 1px)',
          backgroundSize: '72px 72px',
          maskImage: 'radial-gradient(ellipse 70% 60% at 50% 45%, black, transparent 80%)',
          WebkitMaskImage: 'radial-gradient(ellipse 70% 60% at 50% 45%, black, transparent 80%)',
        }}
      />
      <motion.div
        className="absolute inset-[-10%]"
        style={reducedMotion ? undefined : { x: parallaxX, y: parallaxY }}
      >
        <div
          className="h-full w-full"
          style={{
            background:
              'radial-gradient(ellipse 40% 45% at 22% 40%, rgba(31,163,255,0.10), transparent 70%), radial-gradient(ellipse 40% 45% at 80% 55%, rgba(255,138,31,0.09), transparent 70%)',
          }}
        />
      </motion.div>
      {!reducedMotion && (
        <motion.div className="absolute inset-0" style={{ background: pointerGlow }} />
      )}
    </div>
  )
}
