import type { ACPAgentID } from '@/constants'

// Real brand marks for the providers, replacing the C/A/O/X letter avatars.
// Claude surfaces (`claude-code`, `anthropic`) → the Claude sunburst; OpenAI
// surfaces (`codex`, `openai`) → the OpenAI knot; `grok` → the Grok mark; `glm` → the Z.ai mark.
// Grok and Z.ai paths: @lobehub/icons-static-svg 1.102.0 (MIT), grok.svg / zai.svg.

const CLAUDE_CLAY = '#d97757'

const MONO_PATHS: Partial<Record<ACPAgentID, string>> = {
  grok: 'M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815',
  glm: 'M12.105 2L9.927 4.953H.653L2.83 2h9.276zM23.254 19.048L21.078 22h-9.242l2.174-2.952h9.244zM24 2L9.264 22H0L14.736 2H24z'
}

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
  const mono = OPENAI_AGENTS.has(agentId) ? OPENAI_PATH : MONO_PATHS[agentId]
  if (mono) {
    return (
      <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="currentColor"
        aria-hidden="true"
        style={{ display: 'block' }}
      >
        <path d={mono} fillRule={MONO_PATHS[agentId] ? "evenodd" : undefined} />
      </svg>
    )
  }
  return <span style={{ fontSize: size * 0.7, fontWeight: 700 }}>{fallback ?? '?'}</span>
}
