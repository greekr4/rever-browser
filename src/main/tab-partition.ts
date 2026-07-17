// Per-tab session isolation. Each browser tab lives in its own persistent
// Electron partition so it can carry an independent proxy AND an independent
// cookie/storage jar. The partition string is derived purely from the tab id,
// so the renderer (which owns tab ids) and the main process agree without
// having to pass the full partition around.
//
// IMPORTANT: the renderer builds the same string inline as
// `persist:rever-${tab.id}` in WebviewTab.tsx — keep the two in sync.

export const PARTITION_PREFIX = 'persist:rever-'

export function partitionForTab(tabId: string): string {
  return `${PARTITION_PREFIX}${tabId}`
}

// Features that act on "the current tab" via the Electron `session.cookies` API
// (sticky-cookie persistence, Chrome cookie import) need to know which tab is
// active. The renderer reports it on every active-tab change; until then we
// default to the first tab's partition, which is deterministic because the
// renderer's tab-id counter restarts at `t1` on every launch.
let activePartition = partitionForTab('t1')

export function setActivePartition(partition: string): void {
  activePartition = partition
}

export function getActivePartition(): string {
  return activePartition
}
