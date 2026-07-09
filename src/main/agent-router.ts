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
import {
  cancelOpenAiSession,
  getOpenAiModelState,
  isOpenAiSession,
  killOpenAiSession,
  promptOpenAiSession,
  setOpenAiModel,
  spawnOpenAiSession
} from './providers/openai-provider'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse,
  SessionNotification
} from '@agentclientprotocol/sdk'

// spawn/prompt/cancel/kill/model-state/set-model 을 provider별로 라우팅한다.
// agentDef.id 'anthropic'/'openai' 이면 각 API 직접 연동, 그 외에는 기존 ACP.
// 이후 호출은 sessionId 프리픽스('anthropic:'/'openai:')로 구분한다.

export async function spawnSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  if (agentDef.id === 'anthropic') return spawnAnthropicSession()
  if (agentDef.id === 'openai') return spawnOpenAiSession()
  return spawnAcpSession(agentDef, cwd)
}

export async function promptSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void,
  requestPermission?: (req: RequestPermissionRequest) => Promise<RequestPermissionResponse>
): Promise<{ stopReason: string }> {
  if (isAnthropicSession(sessionId)) return promptAnthropicSession(sessionId, text, onUpdate)
  if (isOpenAiSession(sessionId)) return promptOpenAiSession(sessionId, text, onUpdate)
  return promptAcpSession(sessionId, text, onUpdate, requestPermission)
}

export async function cancelSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return cancelAnthropicSession(sessionId)
  if (isOpenAiSession(sessionId)) return cancelOpenAiSession(sessionId)
  return cancelAcpSession(sessionId)
}

export async function killSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return killAnthropicSession(sessionId)
  if (isOpenAiSession(sessionId)) return killOpenAiSession(sessionId)
  return killAcpSession(sessionId)
}

export function sessionModelState(
  sessionId: string
): { availableModels: Array<{ modelId: string; name: string; description?: string | null }>; currentModelId: string | null } | null {
  if (isAnthropicSession(sessionId)) return getAnthropicModelState(sessionId)
  if (isOpenAiSession(sessionId)) return getOpenAiModelState(sessionId)
  return getSessionModelState(sessionId)
}

export async function setSessionModelRouted(sessionId: string, modelId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return setAnthropicModel(sessionId, modelId)
  if (isOpenAiSession(sessionId)) return setOpenAiModel(sessionId, modelId)
  return setSessionModel(sessionId, modelId)
}

export type { AgentDef }
