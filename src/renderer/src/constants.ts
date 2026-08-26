export type ACPAgentID = 'claude-code' | 'codex' | 'anthropic' | 'openai'

export interface ACPAgentDef {
  id: ACPAgentID
  name: string
  /** Primary CLI binary to look for on PATH. */
  command: string
  /** Drop-in forks tried if `command` isn't on PATH. */
  fallbackBins?: string[]
  /** Argv passed to the binary at spawn time. */
  args: string[]
  /** True if this binary speaks ACP and can drive our MCP tool loop. */
  acpSupported: boolean
  /**
   * How the agent loop runs. 'acp' spawns an external ACP binary; 'anthropic'
   * and 'openai' call their respective APIs directly in-process and are gated on
   * an API key instead of a PATH binary.
   */
  provider?: 'acp' | 'anthropic' | 'openai'
  /** Short hint shown in the picker when the binary isn't found. */
  installHint: string
  /** Command that signs the user in, shown when the binary is installed but unauthenticated. */
  loginHint?: string
  /** Single character used in the picker tile. */
  icon: string
}

export const ACP_AGENTS: ACPAgentDef[] = [
  {
    id: 'anthropic',
    name: 'Claude (API)',
    command: '',
    args: [],
    acpSupported: true,
    provider: 'anthropic',
    installHint: 'Add an Anthropic API key in settings',
    icon: 'A'
  },
  {
    id: 'openai',
    name: 'OpenAI (API)',
    command: '',
    args: [],
    acpSupported: true,
    provider: 'openai',
    installHint: 'Add an OpenAI API key in settings',
    icon: 'O'
  },
  {
    id: 'claude-code',
    name: 'Claude Code',
    command: 'claude-agent-acp',
    fallbackBins: ['claude-code-acp'],
    args: [],
    acpSupported: true,
    provider: 'acp',
    installHint: 'npm i -g @agentclientprotocol/claude-agent-acp',
    loginHint: 'claude login',
    icon: 'C'
  },
  {
    id: 'codex',
    name: 'Codex',
    command: 'codex-acp',
    args: [],
    acpSupported: true,
    installHint: 'npm i -g @agentclientprotocol/codex-acp',
    loginHint: 'codex login',
    icon: 'X'
  }
]

export const ACP_PERMISSION_TIMEOUT_MS = 60_000

/**
 * One selectable model in the unified picker. Each row carries
 * the agent it routes to, so choosing a model implicitly selects the provider.
 *
 * For API providers (`anthropic`/`openai`) the `modelId` is exact and applied
 * directly. For subscription ACP agents (`claude-code`/`codex`) the real model
 * ids are only known once a session spawns, so `modelId` here is a best-effort
 * default the picker reconciles against the live list by id-or-name match.
 */
export interface CatalogModel {
  agentId: ACPAgentID
  modelId: string
  name: string
}

export const MODEL_CATALOG: CatalogModel[] = [
  // Claude Code (existing subscription) — reconciled against the live ACP list.
  { agentId: 'claude-code', modelId: 'claude-fable-5', name: 'Claude Fable 5' },
  { agentId: 'claude-code', modelId: 'claude-opus-5', name: 'Claude Opus 5' },
  { agentId: 'claude-code', modelId: 'claude-sonnet-5', name: 'Claude Sonnet 5' },
  { agentId: 'claude-code', modelId: 'claude-haiku-4-5', name: 'Claude Haiku 4.5' },
  { agentId: 'claude-code', modelId: 'claude-opus-4-8', name: 'Claude Opus 4.8' },
  // Codex (existing subscription) — reconciled against the live ACP list.
  { agentId: 'codex', modelId: 'gpt-5.6', name: 'GPT-5.6' },
  { agentId: 'codex', modelId: 'gpt-5.6-mini', name: 'GPT-5.6 Mini' },
  // Claude API — exact ids (mirror of ANTHROPIC_MODELS in the main process).
  { agentId: 'anthropic', modelId: 'claude-opus-4-8', name: 'Claude Opus 4.8' },
  { agentId: 'anthropic', modelId: 'claude-sonnet-5', name: 'Claude Sonnet 5' },
  { agentId: 'anthropic', modelId: 'claude-haiku-4-5', name: 'Claude Haiku 4.5' },
  { agentId: 'anthropic', modelId: 'claude-fable-5', name: 'Claude Fable 5' },
  // OpenAI API — exact ids (mirror of OPENAI_MODELS in the main process).
  { agentId: 'openai', modelId: 'gpt-4o', name: 'GPT-4o' },
  { agentId: 'openai', modelId: 'gpt-4o-mini', name: 'GPT-4o mini' },
  { agentId: 'openai', modelId: 'o3', name: 'o3' },
  { agentId: 'openai', modelId: 'o4-mini', name: 'o4-mini' }
]
