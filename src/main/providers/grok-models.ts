// Curated xAI chat models for the in-app agent. Image, video, and voice
// models are not chat-agent models and stay out of this list.
// Ids: https://docs.x.ai/docs/models — Grok 4.7 is the code and chat flagship.
// Chat Completions at https://api.x.ai/v1 is the wire format this app already
// runs for OpenAI, including the 128-tool cap.

export const XAI_BASE_URL = 'https://api.x.ai/v1'

/** Official JS sample uses 360s so a reasoning completion is not cut off. */
export const GROK_CHAT_TIMEOUT_MS = 360_000

export const GROK_SESSION_PREFIX = 'grok:'

export const GROK_DEFAULT_MODEL = 'grok-4.7'

export interface GrokModelInfo {
  modelId: string
  name: string
  description?: string | null
}

export const GROK_MODELS: readonly GrokModelInfo[] = [
  { modelId: 'grok-4.7', name: 'Grok 4.7', description: 'Flagship for code and chat' },
  { modelId: 'grok-4.6', name: 'Grok 4.6', description: '500k context' },
  { modelId: 'grok-4.5', name: 'Grok 4.5', description: '500k context' },
  { modelId: 'grok-4.3', name: 'Grok 4.3', description: '1M context' }
]

export function isGrokSessionId(sessionId: string): boolean {
  return sessionId.startsWith(GROK_SESSION_PREFIX)
}
