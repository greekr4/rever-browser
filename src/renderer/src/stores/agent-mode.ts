import { create } from 'zustand'

// Which agent surface the right-hand pane shows:
//  - 'acp' : the structured in-app chat over the ACP transport (default)
//  - 'cli' : a real terminal running a local CLI agent (Claude Code) wired to
//            rever's MCP server.
export type AgentMode = 'acp' | 'cli'

const KEY = 'rev:agent-mode'

interface AgentModeState {
  mode: AgentMode
  setMode: (mode: AgentMode) => void
}

export const useAgentModeStore = create<AgentModeState>((set) => ({
  mode: (localStorage.getItem(KEY) as AgentMode) || 'acp',
  setMode: (mode) => {
    localStorage.setItem(KEY, mode)
    set({ mode })
  }
}))
