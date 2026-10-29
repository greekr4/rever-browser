// Signals that a Red Team scan is running so the permission layer
// (acp-permission.ts) can auto-approve risky probe tools without prompting —
// but ONLY for the scan's own scoped site. Out-of-scope requests still prompt,
// and the main-process scope check (mcp/scope.ts) blocks them regardless.

function isLoopbackOrIp(host: string): boolean {
  return host === 'localhost' || /^\d{1,3}(\.\d{1,3}){3}$/.test(host)
}

// Rough eTLD+1 (mirrors mcp/scope.ts) for a bare hostname (no port).
function regDomain(host: string): string {
  if (!host || isLoopbackOrIp(host)) return host
  const p = host.split('.')
  return p.length <= 2 ? host : p.slice(-2).join('.')
}

// The scope key. Loopback / IP share one hostname across apps on different
// ports (localhost:8080 vs :8090), so the port is load-bearing there; public
// domains fold to eTLD+1. Mirrors mcp/scope.ts `scopeKey` on the renderer side.
export function siteKey(input: string): string {
  try {
    const u = new URL(input)
    const reg = regDomain(u.hostname)
    return u.port && isLoopbackOrIp(u.hostname) ? `${reg}:${u.port}` : reg
  } catch {
    return ''
  }
}

// Same key from a bare host that may carry a port (URL.host form, e.g. what
// risky-tools' extractTargetHost returns: "localhost:8090").
function hostKey(hostMaybePort: string): string {
  const m = /^(.+):(\d+)$/.exec(hostMaybePort)
  const host = m ? m[1] : hostMaybePort
  const port = m ? m[2] : ''
  const reg = regDomain(host)
  return port && isLoopbackOrIp(host) ? `${reg}:${port}` : reg
}

let active = false
let scopeHost = ''

export function setRedTeamScan(on: boolean, target?: string): void {
  active = on
  scopeHost = on ? siteKey(target ?? '') : ''
}

export function isRedTeamActive(): boolean {
  return active
}

// True only while a scan runs AND the request host is the scan's scoped site
// (port-aware for loopback/IP, so localhost:8080 never covers :8090).
export function hostInRedTeamScope(host: string | null): boolean {
  return active && !!host && !!scopeHost && hostKey(host) === scopeHost
}
