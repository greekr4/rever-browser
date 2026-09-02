import type { StoredRequest } from '../traffic-store'

// Pure request-comparison helpers, electron-free so they are unit-tested.

export type Change = {
  type: 'added' | 'removed' | 'changed'
  key: string
  from?: unknown
  to?: unknown
}

export function parseQuery(url: string): Record<string, string> {
  try {
    const u = new URL(url)
    const obj: Record<string, string> = {}
    u.searchParams.forEach((v, k) => {
      obj[k] = v
    })
    return obj
  } catch {
    return {}
  }
}

export function diffObjects(
  label: string,
  a: Record<string, unknown>,
  b: Record<string, unknown>
): Change[] {
  const changes: Change[] = []
  const allKeys = new Set([...Object.keys(a), ...Object.keys(b)])
  for (const k of allKeys) {
    const inA = k in a
    const inB = k in b
    if (!inA) changes.push({ type: 'added', key: `${label}.${k}`, to: b[k] })
    else if (!inB) changes.push({ type: 'removed', key: `${label}.${k}`, from: a[k] })
    else if (JSON.stringify(a[k]) !== JSON.stringify(b[k]))
      changes.push({ type: 'changed', key: `${label}.${k}`, from: a[k], to: b[k] })
  }
  return changes
}

export function tryParseJsonObject(s: string | undefined): Record<string, unknown> | null {
  if (!s) return null
  try {
    const parsed = JSON.parse(s)
    if (typeof parsed === 'object' && parsed !== null) return parsed as Record<string, unknown>
    return null
  } catch {
    return null
  }
}

type DiffInput = Pick<StoredRequest, 'url' | 'requestHeaders' | 'requestPostData'>

/** All query/header/body changes between two requests, in one flat list. */
export function diffRequests(a: DiffInput, b: DiffInput): Change[] {
  const changes: Change[] = []

  changes.push(...diffObjects('query', parseQuery(a.url), parseQuery(b.url)))

  const lower = (h: Record<string, string> | undefined): Record<string, string> =>
    Object.fromEntries(Object.entries(h ?? {}).map(([k, v]) => [k.toLowerCase(), v]))
  changes.push(...diffObjects('headers', lower(a.requestHeaders), lower(b.requestHeaders)))

  const bodyA = tryParseJsonObject(a.requestPostData)
  const bodyB = tryParseJsonObject(b.requestPostData)
  if (bodyA && bodyB) {
    changes.push(...diffObjects('body', bodyA, bodyB))
  } else if (a.requestPostData !== b.requestPostData) {
    changes.push({ type: 'changed', key: 'body', from: a.requestPostData, to: b.requestPostData })
  }
  return changes
}

/** Most common host + first-two-path-segments across requests. */
export function computeApiBase(
  urls: string[],
  top = 5
): Array<{ base: string; count: number }> {
  const counts = new Map<string, number>()
  for (const url of urls) {
    try {
      const u = new URL(url)
      const segments = u.pathname.split('/').filter(Boolean)
      const prefix = segments.slice(0, 2).join('/')
      const key = `${u.protocol}//${u.host}/${prefix}`
      counts.set(key, (counts.get(key) ?? 0) + 1)
    } catch {
      // skip unparseable
    }
  }
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, top)
    .map(([base, count]) => ({ base, count }))
}
