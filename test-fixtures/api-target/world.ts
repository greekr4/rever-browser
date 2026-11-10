/**
 * A small multiplayer "world" over WebSocket (/world), so tampering with frames
 * has visible, checkable effects — unlike the plain echo on /ws.
 *
 * Ground truth
 *   Map              0..100 on both axes
 *   move {x,y}       accepted only inside the map and ≤ 15 units per step;
 *                    otherwise rejected (out_of_bounds / too_fast) and snapped back
 *   chat {text}      broadcast to everyone (1..200 chars)
 *   join {name}      sets your display name
 *   teleport {x,y}   HIDDEN: never sent by the page, accepted with no checks at all
 *   server-only      welcome / pos / chat-from-others / error / leave → client_cannot_send
 */

import type { ServerWebSocket } from 'bun'

export const MAP = 100
const MAX_STEP = 15

interface Player {
  id: string
  name: string
  x: number
  y: number
}

const players = new Map<string, Player>()
let seq = 0

type Reply = Record<string, unknown>
const SERVER_ONLY = new Set(['welcome', 'pos', 'error', 'leave', 'echo', 'hello'])

/** Pure rules: what one client message does to its sender and to everyone. */
export function applyMessage(p: Player, raw: string): { toSender: Reply[]; toAll: Reply[] } {
  const toSender: Reply[] = []
  const toAll: Reply[] = []
  let m: Record<string, unknown>
  try {
    m = JSON.parse(raw)
  } catch {
    return { toSender: [{ type: 'error', code: 'bad_json' }], toAll }
  }
  const type = String(m.type ?? '')
  const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null)

  if (SERVER_ONLY.has(type)) return { toSender: [{ type: 'error', code: 'client_cannot_send', got: type }], toAll }

  if (type === 'join') {
    p.name = String(m.name ?? p.name).slice(0, 24)
    toAll.push({ type: 'pos', id: p.id, name: p.name, x: p.x, y: p.y })
  } else if (type === 'move') {
    const x = num(m.x)
    const y = num(m.y)
    if (x == null || y == null) toSender.push({ type: 'error', code: 'bad_move' })
    else if (x < 0 || y < 0 || x > MAP || y > MAP) {
      toSender.push({ type: 'error', code: 'out_of_bounds', got: { x, y } }, { type: 'pos', id: p.id, name: p.name, x: p.x, y: p.y })
    } else if (Math.hypot(x - p.x, y - p.y) > MAX_STEP) {
      toSender.push({ type: 'error', code: 'too_fast', got: { x, y } }, { type: 'pos', id: p.id, name: p.name, x: p.x, y: p.y })
    } else {
      p.x = x
      p.y = y
      toAll.push({ type: 'pos', id: p.id, name: p.name, x, y })
    }
  } else if (type === 'teleport') {
    // Deliberate flaw for the fixture: no bounds or speed check.
    const x = num(m.x) ?? p.x
    const y = num(m.y) ?? p.y
    p.x = x
    p.y = y
    toAll.push({ type: 'pos', id: p.id, name: p.name, x, y, via: 'teleport' })
  } else if (type === 'chat') {
    const text = typeof m.text === 'string' ? m.text : ''
    if (!text || text.length > 200) toSender.push({ type: 'error', code: 'bad_chat' })
    else toAll.push({ type: 'chat', id: p.id, name: p.name, text })
  } else {
    toSender.push({ type: 'error', code: 'unknown_type', got: type })
  }
  return { toSender, toAll }
}

type WS = ServerWebSocket<{ path: string; playerId?: string }>
const TOPIC = 'world'

export function worldOpen(ws: WS): void {
  const p: Player = { id: `p${++seq}`, name: `player-${seq}`, x: 50, y: 50 }
  players.set(p.id, p)
  ws.data.playerId = p.id
  ws.subscribe(TOPIC)
  ws.send(JSON.stringify({ type: 'welcome', id: p.id, map: MAP, players: [...players.values()] }))
  ws.publish(TOPIC, JSON.stringify({ type: 'pos', id: p.id, name: p.name, x: p.x, y: p.y }))
}

export function worldMessage(ws: WS, msg: string | Buffer): void {
  const p = players.get(ws.data.playerId ?? '')
  if (!p) return
  const { toSender, toAll } = applyMessage(p, String(msg))
  for (const r of toSender) ws.send(JSON.stringify(r))
  for (const r of toAll) {
    const s = JSON.stringify(r)
    ws.send(s)
    ws.publish(TOPIC, s)
  }
}

export function worldClose(ws: WS): void {
  const id = ws.data.playerId
  if (!id) return
  players.delete(id)
  ws.publish(TOPIC, JSON.stringify({ type: 'leave', id }))
}

/** Two bots wander so the world has other players in it. */
export function startBots(publish: (topic: string, msg: string) => void): void {
  const bots: Player[] = [
    { id: 'bot-a', name: 'bot-a', x: 20, y: 30 },
    { id: 'bot-b', name: 'bot-b', x: 75, y: 70 }
  ]
  for (const b of bots) players.set(b.id, b)
  setInterval(() => {
    for (const b of bots) {
      b.x = Math.max(0, Math.min(MAP, Math.round(b.x + (Math.random() * 16 - 8))))
      b.y = Math.max(0, Math.min(MAP, Math.round(b.y + (Math.random() * 16 - 8))))
      publish(TOPIC, JSON.stringify({ type: 'pos', id: b.id, name: b.name, x: b.x, y: b.y }))
    }
  }, 1000)
}
