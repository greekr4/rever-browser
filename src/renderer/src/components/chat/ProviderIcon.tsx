import type { ACPAgentID } from '@/constants'

// Real brand marks for the providers, replacing the C/A/O/X letter avatars.
// Claude surfaces (`claude-code`, `anthropic`) → the Claude sunburst; OpenAI
// surfaces (`codex`, `openai`) → the OpenAI knot.

const CLAUDE_CLAY = '#d97757'

// The Claude mark is a radial burst of tapered blades in alternating long/short
// lengths around a small solid core. Generated (rather than a hand-copied path)
// so it stays crisp at any size.
const CLAUDE_RAY_COUNT = 12
const CLAUDE_BASE_LEN = 9.7
const CLAUDE_HALF_WIDTH = 2.0

// Full body near the base narrowing to a pointed tip.
function bladePath(len: number): string {
  const c = 12
  const tip = c - len
  const hw = CLAUDE_HALF_WIDTH
  return (
    `M ${c} ${c} ` +
    `C ${c - hw} ${c - len * 0.25}, ${c - hw * 0.3} ${c - len * 0.82}, ${c} ${tip} ` +
    `C ${c + hw * 0.3} ${c - len * 0.82}, ${c + hw} ${c - len * 0.25}, ${c} ${c} Z`
  )
}

function ClaudeSpark({ size }: { size: number }) {
  const step = 360 / CLAUDE_RAY_COUNT
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill={CLAUDE_CLAY} aria-hidden="true" style={{ display: 'block' }}>
      {Array.from({ length: CLAUDE_RAY_COUNT }, (_, i) => {
        const len = CLAUDE_BASE_LEN + (i % 2 === 0 ? 1.2 : -1.3)
        return <path key={i} d={bladePath(len)} transform={`rotate(${i * step} 12 12)`} />
      })}
      <circle cx="12" cy="12" r="1.7" />
    </svg>
  )
}

const OPENAI_PATH =
  'M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071.006l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.15a.076.076 0 0 1 .071-.006l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zM8.3 12.863l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z'

const OPENAI_AGENTS = new Set<ACPAgentID>(['openai', 'codex'])
const CLAUDE_AGENTS = new Set<ACPAgentID>(['anthropic', 'claude-code'])

interface ProviderIconProps {
  agentId: ACPAgentID
  /** Icon edge length in px. */
  size?: number
  /** Fallback letter when the provider has no brand mark. */
  fallback?: string
}

export function ProviderIcon({ agentId, size = 16, fallback }: ProviderIconProps) {
  if (CLAUDE_AGENTS.has(agentId)) return <ClaudeSpark size={size} />
  if (OPENAI_AGENTS.has(agentId)) {
    return (
      <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="currentColor"
        aria-hidden="true"
        style={{ display: 'block' }}
      >
        <path d={OPENAI_PATH} />
      </svg>
    )
  }
  return <span style={{ fontSize: size * 0.7, fontWeight: 700 }}>{fallback ?? '?'}</span>
}
