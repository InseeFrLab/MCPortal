import { motion, useReducedMotion } from 'motion/react'
import { EASE, fadeUp } from './motion'

// The gateway drawn as a pair of portals: tool calls leave the client, enter the blue
// portal (search, validation, limits) and come out of the orange one towards the right
// upstream server.

const CY = 210 // vertical axis shared by the client and the two portals
const BLUE_X = 222
const ORANGE_X = 378
const PORTAL_RX = 26
const PORTAL_RY = 92

const SERVER_X = 456
const SERVER_W = 128
const SERVER_H = 44
const SERVERS = [
  { name: 'insee', cy: 90 },
  { name: 'melodi', cy: 170 },
  { name: 'rmes', cy: 250 },
  { name: 'geodata', cy: 330 },
]

const TOOLS = ['search_tools', 'call_tool', 'send_feedback']

// One call every 0.9 s; each call goes to the next server in turn.
const PERIOD = 3.6
const IN_DURATION = 0.9
const OUT_DURATION = 1.0
const GAP = 0.15 // time "inside" the gateway, between the two portals

export function PortalFlow() {
  const reducedMotion = useReducedMotion()

  return (
    <motion.figure className="mx-auto w-full max-w-[40rem]" {...fadeUp(reducedMotion, 0.25, 16)}>
      <svg
        viewBox="0 0 600 420"
        className="h-auto w-full"
        role="img"
        aria-label="An MCP client calls MCPortal, which routes each call to one of several upstream MCP servers."
      >
        <defs>
          <filter id="portal-glow" x="-100%" y="-50%" width="300%" height="200%">
            <feGaussianBlur stdDeviation="7" />
          </filter>
          <radialGradient id="blue-core">
            <stop offset="0%" stopColor="#1fa3ff" stopOpacity="0.28" />
            <stop offset="100%" stopColor="#1fa3ff" stopOpacity="0" />
          </radialGradient>
          <radialGradient id="orange-core">
            <stop offset="0%" stopColor="#ff8a1f" stopOpacity="0.26" />
            <stop offset="100%" stopColor="#ff8a1f" stopOpacity="0" />
          </radialGradient>
          <linearGradient id="tunnel" x1="0" x2="1">
            <stop offset="0%" stopColor="#1fa3ff" stopOpacity="0.5" />
            <stop offset="100%" stopColor="#ff8a1f" stopOpacity="0.5" />
          </linearGradient>
        </defs>

        {/* Client */}
        <rect x="16" y={CY - 38} width="128" height="76" rx="6" className="fill-panel" stroke="rgba(238,242,246,0.14)" />
        <text x="80" y={CY - 6} textAnchor="middle" className="font-mono-label fill-off-white" fontSize="15">
          MCP client
        </text>
        <text x="80" y={CY + 16} textAnchor="middle" className="fill-muted-ink" fontSize="12">
          LLM agent, IDE…
        </text>
        <line x1="144" y1={CY} x2={BLUE_X - PORTAL_RX} y2={CY} stroke="#1fa3ff" strokeOpacity="0.35" strokeDasharray="3 5" />

        {/* Inside the gateway: the "tunnel" between the portals */}
        <line x1={BLUE_X} y1={CY - PORTAL_RY + 14} x2={ORANGE_X} y2={CY - PORTAL_RY + 14} stroke="url(#tunnel)" strokeWidth="1.5" />
        <line x1={BLUE_X} y1={CY + PORTAL_RY - 14} x2={ORANGE_X} y2={CY + PORTAL_RY - 14} stroke="url(#tunnel)" strokeWidth="1.5" />
        <text x="300" y={CY + 6} textAnchor="middle" className="font-display fill-off-white" fontSize="19" fontWeight="700" fontStyle="italic">
          MCPortal
        </text>
        <text x="300" y={CY + PORTAL_RY + 34} textAnchor="middle" className="font-mono-label fill-muted-ink" fontSize="12">
          search · validate · limit · relay
        </text>

        <Portal cx={BLUE_X} color="#1fa3ff" core="url(#blue-core)" reverse={false} />
        <Portal cx={ORANGE_X} color="#ff8a1f" core="url(#orange-core)" reverse />

        {/* Pixels flying off the portals, as on the logo */}
        <g fill="#1fa3ff">
          <rect x="250" y="104" width="7" height="7" opacity="0.8" />
          <rect x="262" y="94" width="5" height="5" opacity="0.5" />
          <rect x="252" y="300" width="6" height="6" opacity="0.7" />
        </g>
        <g fill="#ff8a1f">
          <rect x="340" y="100" width="6" height="6" opacity="0.7" />
          <rect x="330" y="298" width="7" height="7" opacity="0.8" />
          <rect x="344" y="308" width="5" height="5" opacity="0.5" />
        </g>

        {/* Upstream servers */}
        {SERVERS.map((server) => (
          <g key={server.name}>
            <path
              d={`M${ORANGE_X + PORTAL_RX} ${CY} C 430 ${CY}, 430 ${server.cy}, ${SERVER_X} ${server.cy}`}
              fill="none"
              stroke="#ff8a1f"
              strokeOpacity="0.3"
              strokeDasharray="3 5"
            />
            <rect
              x={SERVER_X}
              y={server.cy - SERVER_H / 2}
              width={SERVER_W}
              height={SERVER_H}
              rx="6"
              className="fill-panel"
              stroke="rgba(238,242,246,0.14)"
            />
            <circle cx={SERVER_X + 18} cy={server.cy} r="3.5" fill="#ff8a1f" />
            <text x={SERVER_X + 32} y={server.cy + 5} className="font-mono-label fill-off-white" fontSize="14">
              {server.name}
            </text>
          </g>
        ))}

        {!reducedMotion &&
          SERVERS.map((server, i) => <Call key={server.name} targetY={server.cy} delay={i * (PERIOD / SERVERS.length)} />)}
      </svg>

      <figcaption className="mt-[clamp(0.75rem,2vh,1.25rem)] flex flex-wrap items-center justify-center gap-2">
        <span className="font-mono-label mr-1 text-[0.6875rem] tracking-[0.15em] text-muted-ink">
          EXPOSED TO THE CLIENT
        </span>
        {TOOLS.map((tool) => (
          <code
            key={tool}
            className="font-mono-label rounded-sm border border-off-white/10 bg-panel/80 px-2 py-0.5 text-xs text-off-white/90"
          >
            {tool}
          </code>
        ))}
      </figcaption>
    </motion.figure>
  )
}

function Portal({ cx, color, core, reverse }: { cx: number; color: string; core: string; reverse: boolean }) {
  return (
    <g>
      <ellipse cx={cx} cy={CY} rx={PORTAL_RX + 4} ry={PORTAL_RY + 4} fill="none" stroke={color} strokeWidth="8" opacity="0.55" filter="url(#portal-glow)" />
      <ellipse cx={cx} cy={CY} rx={PORTAL_RX} ry={PORTAL_RY} fill={core} stroke={color} strokeWidth="4" />
      <ellipse
        cx={cx}
        cy={CY}
        rx={PORTAL_RX - 8}
        ry={PORTAL_RY - 12}
        fill="none"
        stroke={color}
        strokeOpacity="0.6"
        strokeWidth="2"
        strokeDasharray="22 18"
        className={reverse ? 'portal-swirl-reverse' : 'portal-swirl'}
      />
    </g>
  )
}

// One tool call: a blue pixel that enters the blue portal, then an orange pixel that leaves
// the orange portal for its server. Both legs share the same period, so they stay in step.
function Call({ targetY, delay }: { targetY: number; delay: number }) {
  const outStart = delay + IN_DURATION + GAP
  const midX = 430
  const midY = (CY + targetY) / 2

  return (
    <>
      <motion.g
        initial={{ x: 146, y: CY, opacity: 0 }}
        animate={{ x: [146, BLUE_X - 4], y: CY, opacity: [0, 1, 1, 0] }}
        transition={{
          duration: IN_DURATION,
          delay,
          ease: 'easeIn',
          repeat: Infinity,
          repeatDelay: PERIOD - IN_DURATION,
          opacity: { duration: IN_DURATION, delay, times: [0, 0.15, 0.8, 1], repeat: Infinity, repeatDelay: PERIOD - IN_DURATION },
        }}
      >
        <rect x="-4" y="-4" width="8" height="8" fill="#1fa3ff" />
      </motion.g>
      <motion.g
        initial={{ x: ORANGE_X + 4, y: CY, opacity: 0 }}
        animate={{ x: [ORANGE_X + 4, midX, SERVER_X - 6], y: [CY, midY, targetY], opacity: [0, 1, 1, 0] }}
        transition={{
          duration: OUT_DURATION,
          delay: outStart,
          ease: EASE,
          repeat: Infinity,
          repeatDelay: PERIOD - OUT_DURATION,
          opacity: { duration: OUT_DURATION, delay: outStart, times: [0, 0.1, 0.85, 1], repeat: Infinity, repeatDelay: PERIOD - OUT_DURATION },
        }}
      >
        <rect x="-4" y="-4" width="8" height="8" fill="#ff8a1f" />
      </motion.g>
    </>
  )
}
