import { motion, useReducedMotion } from 'motion/react'
import { EASE } from './motion'

export function InstitutionalLogos() {
  const reducedMotion = useReducedMotion()
  const base = import.meta.env.BASE_URL

  return (
    <motion.div
      className="pointer-events-auto flex items-center justify-end pt-4 lg:absolute lg:right-0 lg:bottom-[clamp(1.25rem,3vh,2rem)] lg:pt-0"
      initial={reducedMotion ? false : { opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.6, delay: 0.55, ease: EASE }}
    >
      <a
        href="https://www.insee.fr/fr/accueil"
        target="_blank"
        rel="noopener noreferrer"
        className="leading-none"
        aria-label="INSEE (opens in a new tab)"
      >
        <img
          src={`${base}logos/insee.svg`}
          alt=""
          className="h-[clamp(1.5rem,3vw,2.25rem)] w-auto opacity-60 brightness-0 invert transition-opacity duration-300 hover:opacity-90"
          width={120}
          height={40}
          loading="lazy"
          decoding="async"
        />
      </a>
    </motion.div>
  )
}
