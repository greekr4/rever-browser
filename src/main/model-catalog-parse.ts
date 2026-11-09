// Pure parsing for the live model catalog (electron-free so vitest can load it).
// The lists come from the agents' own CLIs, so they follow CLI updates the way
// Orca's do — see model-catalog.ts for the probes and the cache.

export interface CatalogEntry {
  modelId: string
  name: string
  description?: string | null
  effortLevels?: string[]
  supportsFastMode?: boolean
}

export interface AgentCatalog {
  models: CatalogEntry[]
  fetchedAt: string
}

export interface CatalogFile {
  version: 1
  agents: Record<string, AgentCatalog>
}

type Json = Record<string, any>

/** Claude Code `initialize` control_response → catalog rows. */
export function claudeModelsFromInit(init: Json): CatalogEntry[] {
  const models = Array.isArray(init?.models) ? init.models : []
  return models
    .filter((m: Json) => typeof m?.value === 'string')
    .map((m: Json) => ({
      modelId: m.value,
      name: typeof m.displayName === 'string' ? m.displayName : m.value,
      description: typeof m.description === 'string' ? m.description : null,
      effortLevels: Array.isArray(m.supportedEffortLevels) ? m.supportedEffortLevels : [],
      supportsFastMode: m.supportsFastMode === true
    }))
}

/** `codex app-server` model/list result page → catalog rows (hidden models dropped, default first). */
export function codexModelsFromList(result: Json): CatalogEntry[] {
  const data: Json[] = Array.isArray(result?.data) ? result.data : []
  return data
    .filter((m) => typeof m?.id === 'string' && m.hidden !== true)
    .sort((a, b) => Number(b.isDefault === true) - Number(a.isDefault === true))
    .map((m) => ({
      modelId: m.id,
      name: typeof m.displayName === 'string' ? m.displayName : m.id,
      description: typeof m.description === 'string' ? m.description : null
    }))
}

/** Reject a cache file whose shape we don't recognise instead of trusting it. */
export function parseCatalogFile(raw: string): CatalogFile {
  try {
    const v = JSON.parse(raw)
    if (v?.version === 1 && v.agents && typeof v.agents === 'object') {
      const agents: Record<string, AgentCatalog> = {}
      for (const [id, c] of Object.entries(v.agents as Record<string, Json>)) {
        if (Array.isArray(c?.models) && typeof c.fetchedAt === 'string') agents[id] = { models: c.models, fetchedAt: c.fetchedAt }
      }
      return { version: 1, agents }
    }
  } catch {
    // fall through to an empty catalog
  }
  return { version: 1, agents: {} }
}
