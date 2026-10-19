import { resolveGrokCredential } from '../grok-login'
import { getApiKey } from '../settings'
import { createChatCompletionsProvider } from './chat-completions-provider'
import {
  GROK_CHAT_TIMEOUT_MS,
  GROK_DEFAULT_MODEL,
  GROK_MODELS,
  GROK_SESSION_PREFIX,
  XAI_BASE_URL,
  isGrokSessionId
} from './grok-models'

// xAI Grok via Chat Completions. A `grok login` subscription is preferred.
// A pasted key or XAI_API_KEY is only the fallback. Same direct catalog and
// 128-tool cap as OpenAI.

async function resolveGrokApiKey(): Promise<string | null> {
  return (await resolveGrokCredential(getApiKey('grok')))?.token ?? null
}

const provider = createChatCompletionsProvider({
  label: 'Grok',
  sessionPrefix: GROK_SESSION_PREFIX,
  defaultModel: GROK_DEFAULT_MODEL,
  models: GROK_MODELS,
  keyProvider: 'grok',
  baseURL: XAI_BASE_URL,
  timeoutMs: GROK_CHAT_TIMEOUT_MS,
  resolveApiKey: resolveGrokApiKey,
  missingKeyError: 'Not signed in to Grok. Run `grok login`, or add an xAI API key.',
  logTag: 'grok'
})

export const spawnGrokSession = provider.spawn
export const promptGrokSession = provider.prompt
export const cancelGrokSession = provider.cancel
export const killGrokSession = provider.kill
export const getGrokModelState = provider.modelState
export const setGrokModel = provider.setModel

export function isGrokSession(sessionId: string): boolean {
  return isGrokSessionId(sessionId)
}
