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

// A pure-negative result ("I checked X and it's fine / not a vuln") is scan
// noise, not a deliverable — a weak worker floods the report with "static CSS
// served without auth = expected" notes instead of the one real vuln. Workers
// label these themselves: `false_positive` status, or a `negative` tag. Enforce
// deterministically so prose ("don't record negatives") a model can ignore
// becomes a rule it can't. The report carries only confirmed/candidate vulns.
export function isScanNoise(f: { status: string; tags: string[] }): boolean {
  return f.status === 'false_positive' || f.tags.some((t) => t.toLowerCase() === 'negative')
}

/** host(:port)+path of the first URL in the text, or first bare-path form. */
function hostPathOf(text: string): string | null {
  const url = text.match(/https?:\/\/[^\s`)\]"'<>]+/i)
  if (url) {
    try {
      const u = new URL(url[0])
      return `${u.hostname.toLowerCase()}${u.port ? `:${u.port}` : ''}${u.pathname.replace(/\/+$/, '') || '/'}`
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
 * Identity of an endpoint finding: host + path of the first URL in the
 * title/body (query stripped), or the first bare absolute path when no URL
 * appears. null = no key (never deduped) — only 'endpoint' inventory entries
 * are keyed here; vuln findings get their own class-scoped key below.
 */
export function endpointKeyOf(f: Pick<DedupableFinding, 'title' | 'body' | 'category'>): string | null {
  if (f.category !== 'endpoint') return null
  return hostPathOf(`${f.title}\n${f.body}`)
}

// Class of a vuln claim, matched on title + tags. Order matters: specific
// classes before generic ones ("Header Injection" is crlf, not headers).
const VULN_CLASS_PATTERNS: [string, RegExp][] = [
  ['cmdi', /command.?inject|\bcmdi\b|\brce\b/i],
  ['ssti', /template.?inject|\bssti\b/i],
  ['sqli', /sql.?inject|\bsqli\b/i],
  ['ssrf', /\bssrf\b|server.?side.?request/i],
  ['lfi', /\blfi\b|path.?traversal|local.?file.?incl/i],
  ['idor', /\bidor\b|\bbola\b|broken.?object/i],
  ['crlf', /\bcrlf\b|header.?inject/i],
  ['redirect', /open.?redirect/i],
  ['pathwrite', /arbitrary.?file.?write|path.?travers|file.?write/i],
  ['headers', /security[\s\w-]{0,20}?headers?\b|missing[^\n]*headers?\b|response[\s-]?header|header[\s-]?posture|\bcsp\b|content.?security|hsts|x.?frame|nosniff|clickjack/i],
  ['cors', /\bcors\b|wildcard.?origin|access.?control.?allow.?origin|\bacao\b/i],
  ['csrf', /\bcsrf\b|cross.?site.?request.?forgery|state.?chang|content.?injection|content.?spoof/i],
  ['xss', /\bxss\b|cross.?site.?script/i],
  ['accesscontrol', /\bacl\b|access.?control|path.?normaliz|path.?confus|proxy.?(deny|bypass)|haproxy|deny.?rule/i],
  ['jwt', /\bjwt\b|token.?forge|\bjws\b|alg.?confusion|python_jwt|jwcrypto/i],
  ['auth', /broken.?auth|auth.?bypass|unauth/i],
  ['secret', /\bsecret\b|api.?key|credential|token.?leak/i]
]

function vulnClassOf(f: Pick<DedupableFinding, 'title' | 'tags'>): string | null {
  const text = `${f.title} ${f.tags.join(' ')}`
  for (const [cls, re] of VULN_CLASS_PATTERNS) if (re.test(text)) return cls
  return null
}

// The injection site: backticked word before "param(parameter)", ?param= in the
// title, or param:"x" style. '' when not stated — same class+endpoint with no
// param stated merges (the claim is the same sink).
function vulnParamOf(title: string): string {
  return (
    title.match(/[`'"](\w{2,})[`'"]\s*\bparam(?:eter)?\b/i)?.[1] ??
    title.match(/[?&](\w+)=/)?.[1] ??
    title.match(/\bparam(?:eter)?\s*[:=]\s*"?(\w+)"?/i)?.[1] ??
    ''
  )
}

/**
 * Identity of a vuln finding: vulnerability class + endpoint + param. N probe
 * workers each validate the same sink (e.g. reflected XSS on ?format=) and
 * report it with different titles — class|endpoint|param is the shared claim.
 * Covers 'vuln' and 'other' (misconfig/headers/CSRF land in 'other' and dupe
 * just as often). null = never deduped: unknown class, or auth/secret/endpoint
 * categories whose same-endpoint findings stay separate claims.
 */
export function vulnKeyOf(
  f: Pick<DedupableFinding, 'title' | 'body' | 'category' | 'tags'>
): string | null {
  if (f.category !== 'vuln' && f.category !== 'other') return null
  const cls = vulnClassOf(f)
  if (!cls) return null
  const ep = hostPathOf(`${f.title}\n${f.body}`)
  if (!ep) return null
  // Host-wide issues (no security headers, wildcard CORS) are a property of the
  // server, not one route — workers report them once per endpoint they touch.
  // Key by host alone so N endpoints collapse to one (merge keeps max severity,
  // so a real CORS-on-login medium isn't lost in an asset-CORS low).
  if (HOST_WIDE_CLASSES.has(cls)) return `vulnlike|${cls}|${ep.split('/')[0] || ep}`
  return `vulnlike|${cls}|${ep}|${vulnParamOf(f.title)}`
}

const HOST_WIDE_CLASSES = new Set(['headers', 'cors'])

/**
 * Merge findings that share a key (endpoint inventory: host+path; vuln claims:
 * class+endpoint+param): the longest body becomes the primary, evidence/tags
 * union, highest severity wins, earliest createdAt keeps. Everything else
 * passes through untouched. Order is preserved (first occurrence position).
 */
function absorbFinding<T extends DedupableFinding>(prev: T, f: T): T {
  const [primary, absorbed] = f.body.length > prev.body.length ? [f, prev] : [prev, f]
  return {
    ...primary,
    severity:
      SEVERITY_RANK[primary.severity] >= SEVERITY_RANK[absorbed.severity] ? primary.severity : absorbed.severity,
    status: primary.status === 'validated' || absorbed.status === 'validated' ? 'validated' : primary.status,
    tags: [...new Set([...primary.tags, ...absorbed.tags])],
    evidenceIds: [
      ...new Set([
        ...primary.evidenceIds,
        ...absorbed.evidenceIds,
        ...(primary.requestId ? [primary.requestId] : []),
        ...(absorbed.requestId ? [absorbed.requestId] : [])
      ])
    ].slice(0, 12),
    createdAt: Math.min(primary.createdAt, absorbed.createdAt)
  }
}

const evidenceIdsOf = (f: DedupableFinding): string[] => [...(f.requestId ? [f.requestId] : []), ...f.evidenceIds]

// Two findings are DIFFERENT claims only when both carry a known, distinct vuln
// class — then a shared evidence id (a request both cite) must not merge them
// (e.g. an ACL bypass and a JWT forgery that both reference /get_ticket).
function classConflict(a: DedupableFinding, b: DedupableFinding): boolean {
  const ca = vulnClassOf(a)
  const cb = vulnClassOf(b)
  return ca !== null && cb !== null && ca !== cb
}

export function mergeDuplicateEndpoints<T extends DedupableFinding>(list: T[]): { merged: T[]; mergedAway: number } {
  const reps: T[] = []
  const byKey = new Map<string, number>()
  const byEvidence = new Map<string, number>() // requestId/evidenceId → rep index

  const indexRep = (idx: number): void => {
    const rep = reps[idx]
    const key = endpointKeyOf(rep) ?? vulnKeyOf(rep)
    if (key !== null) byKey.set(key, idx)
    // Evidence index is the fallback signal for vuln-like findings only; endpoint
    // inventory already dedups by host+path and must not absorb a vuln.
    if (rep.category !== 'endpoint') for (const e of evidenceIdsOf(rep)) if (!byEvidence.has(e)) byEvidence.set(e, idx)
  }

  for (const f of list) {
    const key = endpointKeyOf(f) ?? vulnKeyOf(f)
    let idx = key !== null ? byKey.get(key) ?? -1 : -1
    // Fallback: a vuln-like finding sharing a request/evidence id with an earlier
    // one is the same claim restated — unless they are distinctly classed.
    if (idx < 0 && f.category !== 'endpoint') {
      for (const e of evidenceIdsOf(f)) {
        const cand = byEvidence.get(e)
        if (cand !== undefined && !classConflict(reps[cand], f)) {
          idx = cand
          break
        }
      }
    }
    if (idx < 0) {
      reps.push(f)
      indexRep(reps.length - 1)
    } else {
      reps[idx] = absorbFinding(reps[idx], f)
      indexRep(idx)
    }
  }
  return { merged: reps, mergedAway: list.length - reps.length }
}
