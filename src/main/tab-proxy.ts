import { session } from 'electron'

import { partitionForTab } from './tab-partition'

// Per-tab upstream proxy. Electron proxies are configured per `session`, so
// this only works because each tab has its own partition (see tab-partition.ts).
// Changing a tab's proxy is a live `session.setProxy()` call — no webview
// remount is needed.

export interface TabProxyConfig {
  enabled: boolean
  // Proxy server scheme (NOT the URL scheme it proxies). Chromium routes all
  // protocols through it.
  scheme: 'http' | 'https' | 'socks5'
  host: string
  port: number
  // Optional auth. Answered on the 407 challenge via the app 'login' handler;
  // reliable for http/https proxies (SOCKS auth is handled by Chromium and may
  // not surface here).
  username?: string
  password?: string
}

// partition -> config, kept so the 'login' handler can answer proxy auth
// challenges and so we know which sessions currently carry a proxy.
const proxies = new Map<string, TabProxyConfig>()

function proxyRules(cfg: TabProxyConfig): string {
  return `${cfg.scheme}://${cfg.host}:${cfg.port}`
}

// Apply (or clear) a tab's proxy. Pass null / disabled / empty host to go direct.
export async function applyTabProxy(
  tabId: string,
  cfg: TabProxyConfig | null
): Promise<void> {
  const partition = partitionForTab(tabId)
  const ses = session.fromPartition(partition)
  if (!cfg || !cfg.enabled || !cfg.host) {
    proxies.delete(partition)
    await ses.setProxy({ mode: 'direct' })
    return
  }
  proxies.set(partition, cfg)
  await ses.setProxy({
    proxyRules: proxyRules(cfg),
    // Keep localhost/loopback direct so the in-process MCP server and dev
    // tooling aren't forced through the upstream proxy.
    proxyBypassRules: '<local>'
  })
}

// Look up credentials for a proxy 407 challenge by matching the challenged
// webContents' session against our per-partition sessions (fromPartition
// returns a cached instance, so identity comparison is valid).
export function proxyCredentialsForSession(
  ses: Electron.Session
): { username: string; password: string } | null {
  for (const [partition, cfg] of proxies) {
    if (!cfg.username) continue
    if (session.fromPartition(partition) === ses) {
      return { username: cfg.username, password: cfg.password ?? '' }
    }
  }
  return null
}
