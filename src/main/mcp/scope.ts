// Session target-scope guardrail. When a scope is set, active tools
// (navigate / replay / repeater / intruder / burst) refuse targets outside it,
// so an autonomous run can't accidentally hit un-authorized third-party hosts.
//
// Granularity: entries are stored as registrable domains (eTLD+1, approximated),
// so adding "example.com" also permits api.example.com / cdn.example.com — the
// target's own API and CDN subdomains — without over-blocking.
//
// Opt-in: an empty allowlist means NO enforcement (every host allowed), which
// preserves the prior behavior until the user/agent explicitly sets a scope.

// Minimal multi-label public suffixes so eTLD+1 is right for the common ccTLDs.
const TWO_LEVEL_SUFFIXES = new Set([
  'co.uk', 'org.uk', 'ac.uk', 'gov.uk', 'me.uk',
  'co.kr', 'or.kr', 'ne.kr', 'go.kr', 're.kr', 'pe.kr',
  'co.jp', 'or.jp', 'ne.jp', 'go.jp',
  'com.au', 'net.au', 'org.au',
  'com.br', 'com.cn', 'com.tw', 'com.hk', 'com.sg', 'co.in', 'co.nz'
])

let allowed: string[] = []

export function hostnameOf(input: string): string | null {
  const s = input.trim().toLowerCase()
  if (!s) return null
  if (/^[a-z][a-z0-9+.-]*:\/\//.test(s)) {
    try {
      return new URL(s).hostname || null
    } catch {
      return null
    }
  }
  // bare host, optionally with wildcard prefix, path, or port
  const host = s.replace(/^\*\./, '').split('/')[0].split(':')[0]
  // Reject garbage (spaces, empty labels) — only dot-separated host labels.
  if (!host || !/^[a-z0-9-]+(\.[a-z0-9-]+)*$/.test(host)) return null
  return host
}

// IP literals have no registrable domain — folding "10.0.0.1" to its last two
// labels ("0.1") would put every x.x.0.1 address in scope.
function isIpLiteral(hostname: string): boolean {
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(hostname) || hostname.includes(':')
}

export function registrableDomain(hostname: string): string {
  if (isIpLiteral(hostname)) return hostname
  const parts = hostname.split('.').filter(Boolean)
  if (parts.length <= 2) return hostname
  const last2 = parts.slice(-2).join('.')
  if (TWO_LEVEL_SUFFIXES.has(last2)) return parts.slice(-3).join('.')
  return last2
}

function normalize(hosts: string[]): string[] {
  const out: string[] = []
  for (const h of hosts) {
    const hn = hostnameOf(h)
    if (!hn) continue
    const reg = registrableDomain(hn)
    if (!out.includes(reg)) out.push(reg)
  }
  return out
}

// Pure membership test, exported for tests. Empty allowlist = allow all.
export function hostInScope(hostname: string, allowedDomains: string[]): boolean {
  if (allowedDomains.length === 0) return true
  return allowedDomains.includes(registrableDomain(hostname))
}

export function getScope(): { domains: string[]; enabled: boolean } {
  return { domains: [...allowed], enabled: allowed.length > 0 }
}

export function setScope(hosts: string[]): string[] {
  allowed = normalize(hosts)
  return [...allowed]
}

export function addScopeHosts(hosts: string[]): string[] {
  allowed = normalize([...allowed, ...hosts])
  return [...allowed]
}

export function clearScope(): void {
  allowed = []
}

// Returns a human-readable block message when `url`'s host is outside an active
// scope, or null when allowed (or when no scope is set, or url can't be parsed
// — an unparseable target is not blocked, to avoid false positives).
export function scopeBlockForUrl(url: string | undefined): string | null {
  if (!url) return null
  if (allowed.length === 0) return null
  const hn = hostnameOf(url)
  if (!hn) return null
  if (hostInScope(hn, allowed)) return null
  return `Out of scope: ${hn} is not within the allowed scope [${allowed.join(', ')}]. Add it with scope_add if this target is intended.`
}
