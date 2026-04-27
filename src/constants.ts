export type ACPAgentID = 'claude-code' | 'codex' | 'gemini-cli'

export interface ACPAgentDef {
  id: ACPAgentID
  name: string
  command: string
  args: string[]
}

// M0: Claude Code only. Codex / Gemini in M1.
export const ACP_AGENTS: ACPAgentDef[] = [
  { id: 'claude-code', name: 'Claude Code', command: 'claude-agent-acp', args: [] },
  { id: 'codex', name: 'Codex', command: 'codex-acp', args: [] },
  {
    id: 'gemini-cli',
    name: 'Gemini CLI',
    command: 'gemini',
    args: ['--acp', '--approval-mode', 'yolo']
  }
]

export const ACP_PERMISSION_TIMEOUT_MS = 60_000
