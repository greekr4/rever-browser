import { createChatCompletionsProvider, type ModelInfo } from './chat-completions-provider'

// Z.ai GLM via Chat Completions (OpenAI wire format at api.z.ai). Same direct
// catalog and 128-tool cap as the other in-process providers.

export const GLM_BASE_URL = 'https://api.z.ai/api/paas/v4'

export const GLM_MODELS: ModelInfo[] = [
  { modelId: 'glm-5.3', name: 'GLM-5.3', description: 'Flagship, 1M context' },
  { modelId: 'glm-5.3-flash', name: 'GLM-5.3 Flash', description: 'Multimodal, 3× quota' },
  { modelId: 'glm-5.3-flashx', name: 'GLM-5.3 FlashX', description: '200 tok/s' },
  { modelId: 'glm-5.2', name: 'GLM-5.2', description: 'Previous flagship' },
  { modelId: 'glm-4.6', name: 'GLM-4.6', description: 'Legacy' }
]

const provider = createChatCompletionsProvider({
  label: 'GLM',
  sessionPrefix: 'glm:',
  defaultModel: 'glm-5.3',
  models: GLM_MODELS,
  keyProvider: 'glm',
  missingKeyError: 'No Z.ai API key set. Add one in settings (GLM API key).',
  logTag: 'glm',
  baseURL: GLM_BASE_URL
})

export const spawnGlmSession = provider.spawn
export const promptGlmSession = provider.prompt
export const cancelGlmSession = provider.cancel
export const killGlmSession = provider.kill
export const getGlmModelState = provider.modelState
export const setGlmModel = provider.setModel

export function isGlmSession(sessionId: string): boolean {
  return provider.isSession(sessionId)
}
