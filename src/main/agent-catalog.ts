// The only agents the main process will ever spawn or probe. The renderer
// refers to them by id; it never sends a command line over IPC, so a
// compromised renderer cannot turn `acp:spawn` into arbitrary code execution.
// Keep in sync with ACP_AGENTS in src/renderer/src/constants.ts (UI metadata
// such as names, icons and install hints lives there).

export interface CatalogAgent {
  id: string
  provider: 'acp' | 'anthropic' | 'openai'
  /** Primary CLI binary to look for on PATH ('' for in-process API providers). */
  command: string
  /** Drop-in forks tried if `command` isn't on PATH. */
  fallbackBins?: string[]
  /** Argv passed to the binary at spawn time. */
  args: string[]
}

export const AGENT_CATALOG: readonly CatalogAgent[] = [
  { id: 'anthropic', provider: 'anthropic', command: '', args: [] },
  { id: 'openai', provider: 'openai', command: '', args: [] },
  {
    id: 'claude-code',
    provider: 'acp',
    command: 'claude-agent-acp',
    fallbackBins: ['claude-code-acp'],
    args: []
  },
  { id: 'codex', provider: 'acp', command: 'codex-acp', args: [] }
]

export function catalogAgent(id: string): CatalogAgent | undefined {
  return AGENT_CATALOG.find((a) => a.id === id)
}
