export type ACPAgentID = 'claude-code' | 'codex' | 'anthropic'

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
   * calls the Anthropic Messages API directly in-process and is gated on an API
   * key instead of a PATH binary.
   */
  provider?: 'acp' | 'anthropic'
  /** Short hint shown in the picker when the binary isn't found. */
  installHint: string
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
    id: 'claude-code',
    name: 'Claude Code',
    command: 'claude-agent-acp',
    fallbackBins: ['claude-code-acp'],
    args: [],
    acpSupported: true,
    provider: 'acp',
    installHint: 'npm i -g @agentclientprotocol/claude-agent-acp',
    icon: 'C'
  },
  {
    id: 'codex',
    name: 'Codex',
    command: 'codex-acp',
    args: [],
    acpSupported: true,
    installHint: 'npm i -g @agentclientprotocol/codex-acp',
    icon: 'X'
  }
]

export const ACP_PERMISSION_TIMEOUT_MS = 60_000
