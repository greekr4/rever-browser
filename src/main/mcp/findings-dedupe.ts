// Pure finding-dedup helpers — electron-free so vitest can cover them.
// Twenty parallel scan workers each record the same public endpoint; without
// a key the export report repeats one endpoint once per tab that saw it.

export interface DedupableFinding {
  id: string
  title: string
  body: string
  category: string
  severity: string
  status: string
  tags: string[]
  evidenceIds: string[]
  requestId?: string
  createdAt: number
}

const SEVERITY_RANK: Record<string, number> = { critical: 5, high: 4, medium: 3, low: 2, info: 1 }

/**
 * Identity of an endpoint finding: host + path of the first URL in the
 * title/body (query stripped), or the first bare absolute path when no URL
 * appears. null = no key (never deduped) — vuln/auth/secret findings about the
 * same endpoint are different claims, so only 'endpoint' inventory entries are
 * keyed.
 */
export function endpointKeyOf(f: Pick<DedupableFinding, 'title' | 'body' | 'category'>): string | null {
  if (f.category !== 'endpoint') return null
  const text = `${f.title}\n${f.body}`
  const url = text.match(/https?:\/\/[^\s`)\]"'<>]+/i)
  if (url) {
    try {
      const u = new URL(url[0])
      return `${u.hostname.toLowerCase()}${u.pathname.replace(/\/+$/, '') || '/'}`
    } catch {
      return null
    }
  }
  // No absolute URL: workers write titles like "site.com — public API (/api/x)".
  // Combine the first domain-like token with the first bare path so that form
  // still lands on the same key as the full-URL form.
  const bare = text.match(/(?:^|[\s(`"'])(\/[A-Za-z0-9\-_./]{2,})/)
  if (!bare) return null
  const path = bare[1].replace(/\/+$/, '') || '/'
  const domain = text.match(/\b([a-z0-9-]+(?:\.[a-z0-9-]+)+)\b/i)
  return domain ? `${domain[1].toLowerCase()}${path}` : path
}

/**
 * Merge endpoint findings that share a key: the longest body becomes the
 * primary, evidence/tags union, highest severity wins, earliest createdAt
 * keeps. Non-endpoint findings pass through untouched. Order is preserved
 * (first occurrence position).
 */
export function mergeDuplicateEndpoints<T extends DedupableFinding>(list: T[]): { merged: T[]; mergedAway: number } {
  const byKey = new Map<string, T>()
  const out: T[] = []
  for (const f of list) {
    const key = endpointKeyOf(f)
    if (!key) {
      out.push(f)
      continue
    }
    const prev = byKey.get(key)
    if (!prev) {
      byKey.set(key, f)
      out.push(f)
      continue
    }
    // Absorb: the richer body wins as primary; the other's evidence joins it.
    const [primary, absorbed] = f.body.length > prev.body.length ? [f, prev] : [prev, f]
    const mergedResult: T = {
      ...primary,
      severity:
        SEVERITY_RANK[primary.severity] >= SEVERITY_RANK[absorbed.severity]
          ? primary.severity
          : absorbed.severity,
      status: primary.status === 'validated' || absorbed.status === 'validated' ? 'validated' : primary.status,
      tags: [...new Set([...primary.tags, ...absorbed.tags])],
      evidenceIds: [
        ...new Set([
          ...primary.evidenceIds,
          ...absorbed.evidenceIds,
          ...(absorbed.requestId ? [absorbed.requestId] : [])
        ])
      ].slice(0, 12),
      createdAt: Math.min(primary.createdAt, absorbed.createdAt)
    }
    // The merged entry becomes the group's representative so a third duplicate
    // absorbs IT, not the stale original it replaced.
    byKey.set(key, mergedResult)
    out[out.indexOf(prev)] = mergedResult
  }
  return { merged: out, mergedAway: list.length - out.length }
}
