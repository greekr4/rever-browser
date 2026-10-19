import { createChatCompletionsProvider, type ModelInfo } from './chat-completions-provider'

// OpenAI Chat Completions API를 직접 호출하는 provider. 루프는
// chat-completions-provider에 있고, xAI Grok이 같은 와이어 형식을 재사용한다.

export const OPENAI_MODELS: ModelInfo[] = [
  { modelId: 'gpt-4o', name: 'GPT-4o', description: 'Flagship multimodal' },
  { modelId: 'gpt-4o-mini', name: 'GPT-4o mini', description: 'Fast, cheap' },
  { modelId: 'o3', name: 'o3', description: 'Reasoning' },
  { modelId: 'o4-mini', name: 'o4-mini', description: 'Fast reasoning' }
]

const provider = createChatCompletionsProvider({
  label: 'OpenAI',
  sessionPrefix: 'openai:',
  defaultModel: 'gpt-4o',
  models: OPENAI_MODELS,
  keyProvider: 'openai',
  missingKeyError: 'No OpenAI API key set. Add one in settings (OpenAI API key).',
  logTag: 'openai'
})

export const spawnOpenAiSession = provider.spawn
export const promptOpenAiSession = provider.prompt
export const cancelOpenAiSession = provider.cancel
export const killOpenAiSession = provider.kill
export const getOpenAiModelState = provider.modelState
export const setOpenAiModel = provider.setModel

export function isOpenAiSession(sessionId: string): boolean {
  return provider.isSession(sessionId)
}
