import { useMemo } from 'react'
import { create } from 'zustand'

import { MODEL_CATALOG, type ACPAgentID, type CatalogModel } from '@/constants'
import type { ModelCatalog } from '../../../preload'

export type LiveModel = CatalogModel & ModelCatalog['models'][number]

// Model lists read from the agents' own CLIs by the main process (cached on
// disk, refreshed each launch). Agents without a live list use MODEL_CATALOG.
export const useModelCatalog = create<{ live: Partial<Record<ACPAgentID, ModelCatalog['models']>> }>(() => ({ live: {} }))

let started = false
function start(): void {
  if (started) return
  started = true
  const put = (agentId: string, c: ModelCatalog | null): void => {
    if (c?.models.length) useModelCatalog.setState((s) => ({ live: { ...s.live, [agentId]: c.models } }))
  }
  for (const id of ['claude-code', 'codex'] as const) void window.rev.models.catalog(id).then((c) => put(id, c))
  window.rev.models.onUpdated(put)
}

/** An agent's models: the live CLI list when we have one, else the hardcoded fallback. */
export function useAgentModels(agentId: ACPAgentID): LiveModel[] {
  start()
  const live = useModelCatalog((s) => s.live[agentId])
  return useMemo(
    () => (live ? live.map((m) => ({ agentId, ...m })) : MODEL_CATALOG.filter((m) => m.agentId === agentId)),
    [live, agentId]
  )
}

/** Every agent's models (for the new-tab picker). */
export function useAllAgentModels(): LiveModel[] {
  start()
  const live = useModelCatalog((s) => s.live)
  return useMemo(() => {
    const liveIds = new Set(Object.keys(live))
    return [
      ...MODEL_CATALOG.filter((m) => !liveIds.has(m.agentId)),
      ...Object.entries(live).flatMap(([agentId, models]) => (models ?? []).map((m) => ({ agentId: agentId as ACPAgentID, ...m })))
    ]
  }, [live])
}
