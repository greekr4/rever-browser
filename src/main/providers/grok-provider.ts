import { createChatCompletionsProvider } from './chat-completions-provider'
import {
  GROK_CHAT_TIMEOUT_MS,
  GROK_DEFAULT_MODEL,
  GROK_MODELS,
  GROK_SESSION_PREFIX,
  XAI_BASE_URL,
  isGrokSessionId
} from './grok-models'

// xAI Grok via Chat Completions, using a pasted key or XAI_API_KEY.
// Same direct catalog and 128-tool cap as OpenAI.

const provider = createChatCompletionsProvider({
  label: 'Grok',
  sessionPrefix: GROK_SESSION_PREFIX,
  defaultModel: GROK_DEFAULT_MODEL,
  models: GROK_MODELS,
  keyProvider: 'grok',
  baseURL: XAI_BASE_URL,
  timeoutMs: GROK_CHAT_TIMEOUT_MS,
  missingKeyError: 'No xAI API key set. Add one in settings or export XAI_API_KEY.',
  logTag: 'grok'
})

export const spawnGrokSession = provider.spawn
export const promptGrokSession = provider.prompt
export const cancelGrokSession = provider.cancel
export const killGrokSession = provider.kill
export const getGrokModelState = provider.modelState
export const setGrokModel = provider.setModel
export const isGrokSession = isGrokSessionId
