// Signals that a Red Team scan is running so the permission layer
// (acp-permission.ts) can auto-approve risky probe tools without prompting —
// but only for hosts in the scan's scope. The scope grows as the agent
// scope_set/scope_add's the target's own API hosts (e.g. a backend on another
// localhost port), tracked here so the auto-approve matches what the agent (and
// the server-side scope check in mcp/scope.ts) actually enforce.

function isLoopbackOrIp(host: string): boolean {
  return host === 'localhost' || /^\d{1,3}(\.\d{1,3}){3}$/.test(host)
}

// Rough eTLD+1 (mirrors mcp/scope.ts) for a bare hostname (no port).
function regDomain(host: string): string {
  if (!host || isLoopbackOrIp(host)) return host
  const p = host.split('.')
  return p.length <= 2 ? host : p.slice(-2).join('.')
}

// Scope key from a full URL. Loopback / IP keep the port (localhost:8080 vs
// :8090 are distinct apps); public domains fold to eTLD+1. Mirrors scopeKey in
// mcp/scope.ts on the renderer side.
export function siteKey(input: string): string {
  try {
    const u = new URL(input)
    const reg = regDomain(u.hostname)
    return u.port && isLoopbackOrIp(u.hostname) ? `${reg}:${u.port}` : reg
  } catch {
    return ''
  }
}

// Same key from a bare host that may carry a port ("localhost:8090").
function hostKey(hostMaybePort: string): string {
  const m = /^(.+):(\d+)$/.exec(hostMaybePort)
  const host = m ? m[1] : hostMaybePort
  const port = m ? m[2] : ''
  const reg = regDomain(host)
  return port && isLoopbackOrIp(host) ? `${reg}:${port}` : reg
}

// A host or URL (as passed to scope_set/scope_add) → a scope key.
function keyOf(hostOrUrl: string): string {
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(hostOrUrl) ? siteKey(hostOrUrl) : hostKey(hostOrUrl)
}

let active = false
const scope = new Set<string>()

export function setRedTeamScan(on: boolean, target?: string): void {
  active = on
  scope.clear()
  if (on && target) {
    const k = siteKey(target)
    if (k) scope.add(k)
  }
}

// Union-add hosts the agent brought into scope (scope_set/scope_add). Monotonic
// grow: the server enforces replace semantics; the renderer only needs to not
// prompt for anything the agent authorized, so a superset is safe.
export function addRedTeamScope(hostsOrUrls: string[]): void {
  for (const h of hostsOrUrls) {
    const k = keyOf(h)
    if (k) scope.add(k)
  }
}

export function isRedTeamActive(): boolean {
  return active
}

// True while a scan runs AND the request host is within the tracked scope. A
// bare entry ("localhost") covers all its ports; "localhost:8080" only that one.
export function hostInRedTeamScope(host: string | null): boolean {
  if (!active || !host) return false
  const k = hostKey(host)
  if (scope.has(k)) return true
  const bare = k.split(':')[0]
  return bare !== k && scope.has(bare)
}
