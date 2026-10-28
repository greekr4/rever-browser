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
  cancelGrokSession,
  getGrokModelState,
  isGrokSession,
  killGrokSession,
  promptGrokSession,
  setGrokModel,
  spawnGrokSession
} from './providers/grok-provider'
import {
  cancelOpenAiSession,
  getOpenAiModelState,
  isOpenAiSession,
  killOpenAiSession,
  promptOpenAiSession,
  setOpenAiModel,
  spawnOpenAiSession
} from './providers/openai-provider'
import {
  cancelGlmSession,
  getGlmModelState,
  isGlmSession,
  killGlmSession,
  promptGlmSession,
  setGlmModel,
  spawnGlmSession
} from './providers/glm-provider'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse,
  SessionNotification
} from '@agentclientprotocol/sdk'

import { logPrompt, logStop, logUpdate } from './session-log'

// spawn/prompt/cancel/kill/model-state/set-model 을 provider별로 라우팅한다.
// agentDef.id 'anthropic'/'openai'/'grok' 이면 각 API 직접 연동, 그 외에는 기존 ACP.
// 이후 호출은 sessionId 프리픽스('anthropic:'/'openai:'/'grok:')로 구분한다.

export async function spawnSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  if (agentDef.id === 'anthropic') return spawnAnthropicSession()
  if (agentDef.id === 'openai') return spawnOpenAiSession()
  if (agentDef.id === 'grok') return spawnGrokSession()
  if (agentDef.id === 'glm') return spawnGlmSession()
  return spawnAcpSession(agentDef, cwd)
}

export async function promptSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void,
  requestPermission?: (req: RequestPermissionRequest) => Promise<RequestPermissionResponse>
): Promise<{ stopReason: string }> {
  // Single chokepoint for every provider — tap it to persist a masked transcript.
  logPrompt(sessionId, text)
  const tapped = (n: SessionNotification): void => {
    logUpdate(sessionId, n)
    onUpdate(n)
  }
  const route = (): Promise<{ stopReason: string }> => {
    if (isAnthropicSession(sessionId)) return promptAnthropicSession(sessionId, text, tapped)
    if (isOpenAiSession(sessionId)) return promptOpenAiSession(sessionId, text, tapped)
    if (isGrokSession(sessionId)) return promptGrokSession(sessionId, text, tapped)
    if (isGlmSession(sessionId)) return promptGlmSession(sessionId, text, tapped)
    return promptAcpSession(sessionId, text, tapped, requestPermission)
  }
  const res = await route()
  logStop(sessionId, res.stopReason)
  return res
}

export async function cancelSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return cancelAnthropicSession(sessionId)
  if (isOpenAiSession(sessionId)) return cancelOpenAiSession(sessionId)
  if (isGrokSession(sessionId)) return cancelGrokSession(sessionId)
  if (isGlmSession(sessionId)) return cancelGlmSession(sessionId)
  return cancelAcpSession(sessionId)
}

export async function killSession(sessionId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return killAnthropicSession(sessionId)
  if (isOpenAiSession(sessionId)) return killOpenAiSession(sessionId)
  if (isGrokSession(sessionId)) return killGrokSession(sessionId)
  if (isGlmSession(sessionId)) return killGlmSession(sessionId)
  return killAcpSession(sessionId)
}

export function sessionModelState(
  sessionId: string
): { availableModels: Array<{ modelId: string; name: string; description?: string | null }>; currentModelId: string | null } | null {
  if (isAnthropicSession(sessionId)) return getAnthropicModelState(sessionId)
  if (isOpenAiSession(sessionId)) return getOpenAiModelState(sessionId)
  if (isGrokSession(sessionId)) return getGrokModelState(sessionId)
  if (isGlmSession(sessionId)) return getGlmModelState(sessionId)
  return getSessionModelState(sessionId)
}

export async function setSessionModelRouted(sessionId: string, modelId: string): Promise<void> {
  if (isAnthropicSession(sessionId)) return setAnthropicModel(sessionId, modelId)
  if (isOpenAiSession(sessionId)) return setOpenAiModel(sessionId, modelId)
  if (isGrokSession(sessionId)) return setGrokModel(sessionId, modelId)
  if (isGlmSession(sessionId)) return setGlmModel(sessionId, modelId)
  return setSessionModel(sessionId, modelId)
}

export type { AgentDef }
