import { motion, useReducedMotion } from 'motion/react'
import { DOCS_URL, GITHUB_URL, QUICK_START_URL } from '../links'
import { EASE, fadeUp } from './motion'

const STAGGER = 0.08

const secondaryLink = 'group relative text-sm tracking-wide text-off-white/90'

export function Hero() {
  const reducedMotion = useReducedMotion()
  const step = (i: number) => fadeUp(reducedMotion, 0.05 + STAGGER * i)

  return (
    <div className="max-w-xl lg:max-w-[32rem]">
      <motion.p
        className="font-mono-label mb-[clamp(0.75rem,2vh,1.25rem)] text-[clamp(0.625rem,1vw,0.6875rem)] tracking-[0.2em] text-muted-ink"
        {...step(0)}
      >
        OPEN SOURCE · MCP GATEWAY
      </motion.p>
      <motion.h1
        className="font-display mb-[clamp(0.5rem,1.5vh,1rem)] text-[clamp(2.75rem,6.5vw,4.5rem)] leading-[1.05] font-bold tracking-tight italic"
        {...step(1)}
      >
        <span className="text-portal-blue [text-shadow:0_0_28px_rgba(31,163,255,0.45)]">MC</span>
        <span className="text-portal-orange [text-shadow:0_0_28px_rgba(255,138,31,0.4)]">
          Portal
        </span>
      </motion.h1>
      <motion.p
        className="font-display mb-[clamp(0.75rem,2vh,1.25rem)] text-[clamp(1.25rem,2.4vw,1.75rem)] leading-snug font-medium text-off-white/95"
        {...step(2)}
      >
        Many MCP servers. One portal.
      </motion.p>
      <motion.p
        className="mb-[clamp(1.25rem,3vh,2rem)] max-w-md text-[clamp(0.875rem,1.35vw,1rem)] leading-relaxed text-muted-ink"
        {...step(3)}
      >
        A lightweight gateway, written in Rust, that puts every server from your registry behind
        a single endpoint. Clients find the right tool with a search instead of loading dozens of
        tool definitions into their context.
      </motion.p>
      <motion.div className="flex flex-wrap items-center gap-x-8 gap-y-4" {...step(4)}>
        <motion.a
          href={QUICK_START_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center rounded-sm bg-off-white px-6 py-2.5 text-sm font-medium tracking-wide text-lab-black shadow-[0_0_24px_rgba(31,163,255,0.25)]"
          whileHover={reducedMotion ? undefined : { y: -1, opacity: 0.92 }}
          whileTap={reducedMotion ? undefined : { scale: 0.99 }}
          transition={{ duration: 0.25, ease: EASE }}
        >
          Get Started
        </motion.a>
        <a href={DOCS_URL} target="_blank" rel="noopener noreferrer" className={secondaryLink}>
          Architecture
          <Underline color="bg-portal-blue/80" />
        </a>
        <a href={GITHUB_URL} target="_blank" rel="noopener noreferrer" className={secondaryLink}>
          GitHub
          <Underline color="bg-portal-orange/80" />
        </a>
      </motion.div>
    </div>
  )
}

function Underline({ color }: { color: string }) {
  return (
    <span
      className={`absolute -bottom-0.5 left-0 h-px w-0 ${color} transition-all duration-300 group-hover:w-full`}
    />
  )
}
