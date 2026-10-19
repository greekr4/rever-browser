import type { SessionConfigOption } from '@agentclientprotocol/sdk'

export interface ModelInfo {
  modelId: string
  name: string
  description?: string | null
}

export interface SessionModelState {
  availableModels: ModelInfo[]
  currentModelId: string | null
  // Set when the agent exposes the model as a session config option (newer
  // claude-agent-acp); switching then goes through setSessionConfigOption
  // instead of the removed unstable_setSessionModel.
  modelConfigId: string | null
}

interface NewSessionModelFields {
  models?: { availableModels?: ModelInfo[]; currentModelId?: string } | null
  configOptions?: SessionConfigOption[] | null
}

export function modelStateFromSession(result: NewSessionModelFields): SessionModelState {
  const option = result.configOptions?.find((o) => o.category === 'model' && o.type === 'select')
  if (option && option.type === 'select') {
    const flat = option.options.flatMap((o) => ('group' in o ? o.options : [o]))
    return {
      availableModels: flat.map((o) => ({
        modelId: o.value,
        name: o.name,
        description: o.description ?? null
      })),
      currentModelId: option.currentValue,
      modelConfigId: option.id
    }
  }
  return {
    availableModels: result.models?.availableModels ?? [],
    currentModelId: result.models?.currentModelId ?? null,
    modelConfigId: null
  }
}
