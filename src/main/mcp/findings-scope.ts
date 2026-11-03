import { scopeKey } from './scope'

/** Scope key of an http(s) URL (localhost:8080 → "localhost:8080"); undefined otherwise. */
export function originOf(url: string | undefined): string | undefined {
  if (!url) return undefined
  if (!/^https?:\/\//i.test(url)) return undefined
  return scopeKey(url) ?? undefined
}

/**
 * Scope findings to one site. `all` returns everything (incl. legacy findings
 * with no origin). A null/undefined scope returns everything too — better a
 * full report than an empty one when the current site can't be determined.
 */
export function filterByOrigin<T extends { origin?: string }>(
  list: T[],
  scope: string | undefined,
  all: boolean
): T[] {
  if (all || !scope) return list
  return list.filter((f) => f.origin === scope)
}

/**
 * Scope for finding_export: an explicit `site` wins (the scan knows its target
 * even when no tab is attached anymore), then the active page's URL. Neither →
 * error instead of a whole-store dump: the store spans months of sites and a
 * scopeless export once hit 343KB / 392 findings, which the agent cannot even
 * ingest.
 */
export function resolveExportScope(
  site: string | undefined,
  activeUrl: string | undefined,
  all: boolean
): { scope?: string; error?: string } {
  if (all) return {}
  const scope = originOf(site) ?? originOf(activeUrl)
  if (!scope) {
    return {
      error:
        'no scope: pass site with the scanned URL (e.g. site: "http://localhost:1337") or all=true — otherwise I cannot tell which site the report covers.'
    }
  }
  return { scope }
}
