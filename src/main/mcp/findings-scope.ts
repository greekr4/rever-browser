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
