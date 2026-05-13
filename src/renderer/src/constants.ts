export type ACPAgentID =
  | 'claude-code'
  | 'codex'
  | 'gemini-cli'
  | 'cursor-agent'
  | 'opencode'
  | 'qwen'
  | 'copilot'
  | 'devin'
  | 'hermes'
  | 'kimi'
  | 'kiro'
  | 'kilo'
  | 'qoder'
  | 'pi'
  | 'vibe'
  | 'deepseek'

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
  /** Short hint shown in the picker when the binary isn't found. */
  installHint: string
  /** Single character used in the picker tile. */
  icon: string
}

// Catalog mirrors nexu-io/open-design's runtime defs (16 entries) so the
// picker UI shows the same roadmap. Only `acpSupported: true` entries can
// actually drive our MCP tool loop today — the rest render disabled with
// a "Not yet ACP-compatible" badge until non-ACP transports are added.
export const ACP_AGENTS: ACPAgentDef[] = [
  {
    id: 'claude-code',
    name: 'Claude Code',
    command: 'claude-agent-acp',
    fallbackBins: ['claude-code-acp'],
    args: [],
    acpSupported: true,
    installHint: 'npm i -g @agentclientprotocol/claude-agent-acp',
    icon: 'C'
  },
  {
    id: 'codex',
    name: 'Codex',
    command: 'codex-acp',
    args: [],
    acpSupported: true,
    installHint: 'npm i -g codex-acp (when available)',
    icon: 'X'
  },
  {
    id: 'gemini-cli',
    name: 'Gemini CLI',
    command: 'gemini',
    args: ['--acp', '--approval-mode', 'yolo'],
    acpSupported: true,
    installHint: 'npm i -g @google/gemini-cli',
    icon: 'G'
  },
  {
    id: 'cursor-agent',
    name: 'Cursor Agent',
    command: 'cursor-agent',
    args: [],
    acpSupported: false,
    installHint: 'Bundled with Cursor — uses non-ACP stream',
    icon: '◆'
  },
  {
    id: 'opencode',
    name: 'OpenCode',
    command: 'opencode-cli',
    fallbackBins: ['opencode'],
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP JSON stream — planned',
    icon: 'O'
  },
  {
    id: 'qwen',
    name: 'Qwen Code',
    command: 'qwen',
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP stream — planned',
    icon: 'Q'
  },
  {
    id: 'copilot',
    name: 'GitHub Copilot CLI',
    command: 'copilot',
    args: [],
    acpSupported: false,
    installHint: 'gh extension install github/gh-copilot',
    icon: '⌥'
  },
  {
    id: 'devin',
    name: 'Devin (Terminal)',
    command: 'devin',
    args: [],
    acpSupported: false,
    installHint: 'Cognition Devin terminal CLI — planned',
    icon: 'D'
  },
  {
    id: 'hermes',
    name: 'Hermes',
    command: 'hermes',
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP stream — planned',
    icon: 'H'
  },
  {
    id: 'kimi',
    name: 'Kimi CLI',
    command: 'kimi',
    args: [],
    acpSupported: false,
    installHint: 'Moonshot Kimi CLI — planned',
    icon: 'K'
  },
  {
    id: 'kiro',
    name: 'Kiro',
    command: 'kiro',
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP stream — planned',
    icon: 'ʞ'
  },
  {
    id: 'kilo',
    name: 'Kilo',
    command: 'kilo',
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP stream — planned',
    icon: 'k'
  },
  {
    id: 'qoder',
    name: 'Qoder CLI',
    command: 'qoder',
    args: [],
    acpSupported: false,
    installHint: 'Non-ACP stream — planned',
    icon: 'q'
  },
  {
    id: 'pi',
    name: 'Pi',
    command: 'pi',
    args: [],
    acpSupported: false,
    installHint: 'Inflection Pi CLI — planned',
    icon: 'π'
  },
  {
    id: 'vibe',
    name: 'Mistral Vibe',
    command: 'vibe',
    args: [],
    acpSupported: false,
    installHint: 'Mistral Vibe CLI — planned',
    icon: '~'
  },
  {
    id: 'deepseek',
    name: 'DeepSeek TUI',
    command: 'deepseek',
    args: [],
    acpSupported: false,
    installHint: 'DeepSeek TUI — planned',
    icon: 'd'
  }
]

export const ACP_PERMISSION_TIMEOUT_MS = 60_000
