import { app, ipcMain } from 'electron'
import { mkdirSync } from 'node:fs'
import path from 'node:path'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'

import { detectAgents, type AgentProbe } from '../acp-detect'
import { probeAgents } from '../agent-probe'
import { AGENT_CATALOG, catalogAgent } from '../agent-catalog'
import {
  cancelSession,
  killSession,
  promptSession,
  sessionModelState,
  setSessionModelRouted,
  spawnSession,
  type AgentDef
} from '../agent-router'
import { getApiKey, hasApiKey, setApiKey, type ApiProvider } from '../settings'

// A pending permission round-trip to the renderer. Kept module-local: nothing
// outside these handlers touches it.
interface PendingPermission {
  resolve: (r: RequestPermissionResponse) => void
  reject: (e: Error) => void
  timer: NodeJS.Timeout
}

const pendingPermissions = new Map<string, PendingPermission>()
const PERMISSION_GUARD_MS = 65_000
let permissionSeq = 0

// Agent lifecycle + API-key + permission IPC. Extracted from index.ts; the only
// state it owns is the permission map above. Register once at startup.
export function registerAcpIpc(): void {
  ipcMain.handle('acp:list-available', async (_event, probes: AgentProbe[]) => {
    return detectAgents(probes)
  })

  // The renderer never supplies a command line: probe and spawn both resolve
  // the agent from the main-process catalog by id.
  ipcMain.handle('agent:probe', async () => {
    return probeAgents(AGENT_CATALOG.map((a) => ({ ...a })))
  })

  ipcMain.handle('acp:spawn', async (_event, agentId: string, _cwd: string) => {
    const agent = catalogAgent(agentId)
    if (!agent) throw new Error(`unknown agent id: ${agentId}`)
    let command = agent.command
    if (agent.provider === 'acp') {
      const [found] = await detectAgents([
        { command: agent.command, fallbackBins: agent.fallbackBins }
      ])
      if (!found?.resolvedPath)
        throw new Error(`agent "${agentId}" is not installed (${agent.command})`)
      command = found.resolvedPath
    }
    const agentDef: AgentDef = { id: agent.id, command, args: agent.args }
    // Always sandbox the agent in a scratch directory under userData so
    // Edit/Write/Bash tools cannot accidentally mutate the rever-browser
    // source tree. The renderer's cwd hint is intentionally ignored.
    const scratch = path.join(app.getPath('userData'), 'agent-scratch')
    try {
      mkdirSync(scratch, { recursive: true })
    } catch (e) {
      console.warn('[acp:spawn] failed to ensure scratch dir', e)
    }
    return spawnSession(agentDef, scratch)
  })

  ipcMain.handle('settings:get-api-key', (_event, provider: ApiProvider) => getApiKey(provider))
  ipcMain.handle('settings:has-api-key', (_event, provider: ApiProvider) => hasApiKey(provider))
  ipcMain.handle('settings:set-api-key', (_event, provider: ApiProvider, key: string) => {
    setApiKey(provider, key)
    return hasApiKey(provider)
  })

  ipcMain.handle('acp:prompt', async (event, sessionId: string, text: string, channel: string) => {
    const sender = event.sender
    const requestPermission = (
      req: RequestPermissionRequest
    ): Promise<RequestPermissionResponse> =>
      new Promise((resolve, reject) => {
        if (sender.isDestroyed()) {
          reject(new Error('renderer destroyed'))
          return
        }
        const id = `perm:${permissionSeq++}`
        const timer = setTimeout(() => {
          pendingPermissions.delete(id)
          reject(new Error('permission request timed out (renderer not responding)'))
        }, PERMISSION_GUARD_MS)
        pendingPermissions.set(id, { resolve, reject, timer })
        sender.send('acp:permission-request', { id, request: req })
      })
    return promptSession(
      sessionId,
      text,
      (notification) => {
        if (sender.isDestroyed()) return
        sender.send(channel, notification)
      },
      requestPermission
    )
  })

  ipcMain.on(
    'acp:permission-respond',
    (_event, id: string, response: RequestPermissionResponse) => {
      const pending = pendingPermissions.get(id)
      if (!pending) return
      clearTimeout(pending.timer)
      pendingPermissions.delete(id)
      pending.resolve(response)
    }
  )

  ipcMain.handle('acp:cancel', async (_event, sessionId: string) => cancelSession(sessionId))
  ipcMain.handle('acp:kill', async (_event, sessionId: string) => killSession(sessionId))
  ipcMain.handle('acp:model-state', (_event, sessionId: string) => sessionModelState(sessionId))
  ipcMain.handle('acp:set-model', async (_event, sessionId: string, modelId: string) =>
    setSessionModelRouted(sessionId, modelId)
  )
}
