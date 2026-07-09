import {
  cancelAcpSession,
  getSessionModelState,
  killAcpSession,
  promptAcpSession,
  setSessionModel,
  spawnAcpSession,
  type AgentDef
} from './acp-session'
import {
  cancelAnthropicSession,
  getAnthropicModelState,
  isAnthropicSession,
  killAnthropicSession,
  promptAnthropicSession,
  setAnthropicModel,
  spawnAnthropicSession
} from './providers/anthropic-provider'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse,
  SessionNotification
} from '@agentclientprotocol/sdk'

// spawn/prompt/cancel/kill/model-state/set-model 을 provider별로 라우팅한다.
// agentDef.id === 'anthropic' 이면 Anthropic 직접 연동, 그 외에는 기존 ACP.
// 이후 호출은 sessionId 프리픽스('anthropic:')로 구분한다.

export async function spawnSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  if (agentDef.id === 'anthropic') {
    return spawnAnthropicSession()
  }
  return spawnAcpSession(agentDef, cwd)
}

export async function promptSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void,
  requestPermission?: (req: RequestPermissionRequest) => Promise<RequestPermissionResponse>
): Promise<{ stopReason: string }> {
  if (isAnthropicSession(sessionId)) {
    return promptAnthropicSession(sessionId, text, onUpdate)
  }
  return promptAcpSession(sessionId, text, onUpdate, requestPermission)
}

export async function cancelSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return cancelAnthropicSession(sessionId)
  return cancelAcpSession(sessionId)
}

export async function killSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return killAnthropicSession(sessionId)
  return killAcpSession(sessionId)
}

export function sessionModelState(
  sessionId: string
): { availableModels: Array<{ modelId: string; name: string; description?: string | null }>; currentModelId: string | null } | null {
  if (isAnthropicSession(sessionId)) return getAnthropicModelState(sessionId)
  return getSessionModelState(sessionId)
}

export async function setSessionModelRouted(sessionId: string, modelId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return setAnthropicModel(sessionId, modelId)
  return setSessionModel(sessionId, modelId)
}

export type { AgentDef }
