import { useMemo, useState } from 'react'

import { useT } from '@/stores/i18n'
import { useTrafficStore } from '@/stores/traffic'
import type { ApiEndpoint } from '@/types/traffic'

const METHOD_COLOR: Record<string, string> = {
  GET: 'var(--act-scroll, #1f9d55)',
  POST: 'var(--act-evaluate, #c2691a)',
  PUT: 'var(--act-type, #2f6fed)',
  PATCH: 'var(--act-navigate, #8b5cf6)',
  DELETE: 'var(--act-click, #e5484d)'
}

function methodColor(m: string): string {
  return METHOD_COLOR[m] ?? 'var(--text-dim)'
}

// Second-level labels that are part of the public suffix, not the site name:
// mail.naver.co.kr → naver.co.kr, not co.kr.
const PUBLIC_SLD = new Set(['co', 'ne', 'or', 'go', 're', 'pe', 'ac', 'com', 'net', 'org', 'edu', 'gov'])

/** endpoints captured before `host` existed only carry an origin */
function hostOf(ep: { host?: string; origin: string }): string {
  return ep.host || ep.origin.replace(/^https?:\/\//, '').replace(/:\d+$/, '')
}

/** mail.naver.com → naver.com · api.shop.co.kr → shop.co.kr · localhost → localhost */
function baseDomain(host: string): string {
  const parts = host.split('.')
  if (parts.length < 3) return host
  const tld = parts[parts.length - 1]
  const sld = parts[parts.length - 2]
  const take = tld.length <= 3 && PUBLIC_SLD.has(sld) ? 3 : 2
  return parts.slice(-take).join('.')
}

/** the part of the host that is not the base domain — 'mail', or '@' for the apex */
function subLabel(host: string, base: string): string {
  return host === base ? '@' : host.slice(0, host.length - base.length - 1)
}

function clip(s: string, n: number): string {
  return s.length > n ? '…' + s.slice(-(n - 1)) : s
}

/** what a ring node draws — hosts and endpoints share the same shape */
interface MapNode {
  key: string
  badge: string
  color: string
  label: string
  meta: string
  auth: boolean
  title: string
  onClick?: () => void
}

const W = 800
const H = 420
const CX = W / 2
const CY = H / 2

export function ApiMapPanel() {
  const t = useT()
  const endpointMap = useTrafficStore((s) => s.endpoints)
  const entries = useTrafficStore((s) => s.entries)
  const openDetail = useTrafficStore((s) => s.openDetail)

  const endpoints = useMemo(
    () => Object.values(endpointMap).sort((a, b) => b.count - a.count),
    [endpointMap]
  )

  const domains = useMemo(() => {
    const counts = new Map<string, number>()
    for (const ep of endpoints) {
      const d = baseDomain(hostOf(ep))
      counts.set(d, (counts.get(d) ?? 0) + 1)
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1])
  }, [endpoints])

  const [picked, setPicked] = useState<string | null>(null)
  const domain = picked && domains.some(([d]) => d === picked) ? picked : domains[0]?.[0]

  // hosts under the selected domain, busiest first
  const hosts = useMemo(() => {
    if (!domain) return []
    const byHost = new Map<string, ApiEndpoint[]>()
    for (const ep of endpoints) {
      if (baseDomain(hostOf(ep)) !== domain) continue
      const h = hostOf(ep)
      const list = byHost.get(h)
      if (list) list.push(ep)
      else byHost.set(h, [ep])
    }
    return [...byHost.entries()]
      .map(([host, eps]) => ({ host, eps, hits: eps.reduce((n, e) => n + e.count, 0) }))
      .sort((a, b) => b.eps.length - a.eps.length)
  }, [endpoints, domain])

  const [focus, setFocus] = useState<string | null>(null)
  // a single-host domain has nothing to drill into — show its endpoints straight away
  const host = focus && hosts.some((h) => h.host === focus) ? focus : hosts.length === 1 ? hosts[0].host : null

  const [hover, setHover] = useState<string | null>(null)

  const nodes: MapNode[] = useMemo(() => {
    if (!domain) return []
    if (host) {
      const eps = hosts.find((h) => h.host === host)?.eps ?? []
      return eps.slice(0, 20).map((ep) => ({
        key: ep.key,
        badge: ep.method.slice(0, 4),
        color: methodColor(ep.method),
        label: clip(ep.path, 26),
        meta: [`×${ep.count}`, ep.statuses.join('/')].filter(Boolean).join(' · '),
        auth: ep.auth,
        title: `${ep.method} ${ep.origin}${ep.path}`,
        onClick: () => {
          // the entry may have scrolled out of the 500-row ring buffer
          if (entries[ep.lastRequestId]) openDetail(ep.lastRequestId)
        }
      }))
    }
    return hosts.slice(0, 16).map((h) => ({
      key: h.host,
      badge: String(h.eps.length),
      color: 'var(--accent-text)',
      label: clip(subLabel(h.host, domain), 24),
      meta: `${h.eps.length} endpoints · ${h.hits} reqs`,
      auth: h.eps.some((e) => e.auth),
      title: h.host,
      onClick: () => setFocus(h.host)
    }))
  }, [domain, host, hosts, entries, openDetail])

  if (endpoints.length === 0) {
    return (
      <div className="panel-empty">
        No API calls captured yet — browse the target site and XHR/Fetch requests will map here.
      </div>
    )
  }

  const twoRings = nodes.length > 10
  const placed = nodes.map((n, i) => {
    const angle = (i / nodes.length) * Math.PI * 2 - Math.PI / 2
    const r = twoRings ? (i % 2 === 0 ? 175 : 112) : nodes.length <= 6 ? 130 : 155
    return { n, x: CX + Math.cos(angle) * r, y: CY + Math.sin(angle) * r, cos: Math.cos(angle) }
  })

  const centreLabel = host ? subLabel(host, domain ?? '') : (domain ?? '')
  const centreKind = host ? 'HOST' : 'DOMAIN'

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="panel-toolbar">
        <select
          value={domain ?? ''}
          onChange={(e) => {
            setPicked(e.target.value)
            setFocus(null)
          }}
          style={{ fontSize: 11, maxWidth: 260 }}
        >
          {domains.map(([d, n]) => (
            <option key={d} value={d}>
              {d} ({n})
            </option>
          ))}
        </select>

        {host && hosts.length > 1 && (
          <button className="btn-ghost" onClick={() => setFocus(null)}>
            {t('panel.allHosts')}
          </button>
        )}

        <span style={{ opacity: 0.6 }}>
          {host
            ? `${host} · ${nodes.length} endpoints`
            : `${hosts.length} subdomains · ${endpoints.filter((e) => baseDomain(hostOf(e)) === domain).length} endpoints`}
        </span>

        <span style={{ marginLeft: 'auto', display: 'flex', gap: 10, opacity: 0.6 }}>
          {(host ? ['GET', 'POST'] : []).map((m) => (
            <span key={m} style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}>
              <i
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: 2,
                  background: methodColor(m),
                  display: 'inline-block'
                }}
              />
              {m}
            </span>
          ))}
          {!host && <span>click a subdomain to open it</span>}
        </span>
      </div>

      <div style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
        <svg
          viewBox={`0 0 ${W} ${H}`}
          style={{ width: '100%', height: '100%', display: 'block' }}
          preserveAspectRatio="xMidYMid meet"
        >
          <circle cx={CX} cy={CY} r={175} fill="none" stroke="var(--border)" strokeDasharray="2 6" />
          {twoRings && (
            <circle cx={CX} cy={CY} r={112} fill="none" stroke="var(--border)" strokeDasharray="2 6" />
          )}

          {placed.map(({ n, x, y }) => (
            <line
              key={`e-${n.key}`}
              x1={CX}
              y1={CY}
              x2={x}
              y2={y}
              stroke={n.color}
              strokeWidth={1.5}
              opacity={hover && hover !== n.key ? 0.12 : 0.4}
            />
          ))}

          {/* centre card */}
          <rect
            x={CX - 92}
            y={CY - 20}
            width={184}
            height={40}
            rx={10}
            fill="var(--surface)"
            stroke="var(--border-2)"
          />
          <text x={CX} y={CY - 3} textAnchor="middle" fontSize={9} fill="var(--text-dim)" fontFamily="var(--mono, monospace)">
            {centreKind}
          </text>
          <text
            x={CX}
            y={CY + 11}
            textAnchor="middle"
            fontSize={11}
            fontWeight={600}
            fill="var(--text)"
            fontFamily="var(--mono, monospace)"
          >
            {clip(centreLabel, 24)}
          </text>

          {placed.map(({ n, x, y, cos }) => {
            const dim = hover !== null && hover !== n.key
            const anchor = Math.abs(cos) < 0.25 ? 'middle' : cos > 0 ? 'start' : 'end'
            const tx = anchor === 'middle' ? x : cos > 0 ? x + 26 : x - 26
            // above a node the meta line sits below the label, so the pair has to
            // clear the circle by its own height — not just the label's
            const ty = anchor === 'middle' ? (y < CY ? y - 40 : y + 34) : y + 4
            return (
              <g
                key={n.key}
                opacity={dim ? 0.3 : 1}
                style={{ cursor: n.onClick ? 'pointer' : 'default' }}
                onMouseEnter={() => setHover(n.key)}
                onMouseLeave={() => setHover(null)}
                onClick={n.onClick}
              >
                <circle cx={x} cy={y} r={20} fill="var(--surface)" stroke={n.color} strokeWidth={1.6} />
                <text
                  x={x}
                  y={y + 3}
                  textAnchor="middle"
                  fontSize={n.badge.length > 3 ? 8.5 : 10}
                  fontWeight={700}
                  fill={n.color}
                  fontFamily="var(--mono, monospace)"
                >
                  {n.badge}
                </text>
                {n.auth && (
                  <circle cx={x + 15} cy={y - 15} r={6} fill="var(--chip-warn-text, #d9a300)" stroke="var(--surface)" strokeWidth={1.4} />
                )}
                <text x={tx} y={ty} textAnchor={anchor} fontSize={10.5} fill="var(--text)" fontFamily="var(--mono, monospace)">
                  {n.label}
                </text>
                <text x={tx} y={ty + 12} textAnchor={anchor} fontSize={9} fill="var(--text-dim)" fontFamily="var(--mono, monospace)">
                  {n.meta}
                </text>
                <title>{n.title}</title>
              </g>
            )
          })}
        </svg>
      </div>
    </div>
  )
}
