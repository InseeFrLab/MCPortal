import { motion, useReducedMotion } from 'motion/react'
import { InstitutionalLogos } from './InstitutionalLogos'
import { EASE } from './motion'

const FEATURES = [
  'BM25 tool search',
  'JSON Schema validation',
  'Stateless MCP 2026-07-28',
  'Rate limits',
  'Prometheus & JSON logs',
] as const

export function Features() {
  const reducedMotion = useReducedMotion()

  return (
    <motion.footer
      className="relative shrink-0 pt-[clamp(0.75rem,2vh,1.5rem)] pb-[clamp(1.25rem,3vh,2rem)]"
      initial={reducedMotion ? false : { opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.7, delay: 0.35, ease: EASE }}
    >
      <ul className="grid grid-cols-2 gap-x-6 gap-y-3 lg:flex lg:flex-wrap lg:items-center lg:justify-center lg:gap-x-[clamp(2rem,4vw,4rem)] lg:gap-y-2">
        {FEATURES.map((label, index) => (
          <motion.li
            key={label}
            className="text-center text-[clamp(0.7rem,1.1vw,0.8125rem)] tracking-wide text-muted-ink"
            initial={reducedMotion ? false : { opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.55, delay: 0.45 + index * 0.06, ease: EASE }}
          >
            {label}
          </motion.li>
        ))}
      </ul>
      <InstitutionalLogos />
    </motion.footer>
  )
}
