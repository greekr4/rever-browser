import { describe, it, expect, beforeEach } from 'vitest'

import {
  upsertRequest,
  listRequests,
  getRequest,
  clearTraffic,
  appendWsFrame,
  getWsFrames,
  appendConsole,
  getConsoleSince,
  clearConsole,
  appendException,
  getExceptions
} from './traffic-store'

beforeEach(() => {
  clearTraffic()
  clearConsole()
})

describe('upsertRequest / getRequest', () => {
  it('creates a new entry with defaults', () => {
    upsertRequest({ requestId: 'r1', url: 'https://a.com/x', host: 'a.com' })
    const r = getRequest('r1')
    expect(r?.url).toBe('https://a.com/x')
    expect(r?.resourceType).toBe('Other')
    expect(typeof r?.startedAt).toBe('number')
  })

  it('merges fields into an existing entry instead of replacing it', () => {
    upsertRequest({ requestId: 'r1', url: 'https://a.com', host: 'a.com', method: 'GET' })
    upsertRequest({ requestId: 'r1', status: 200, mimeType: 'application/json' })
    const r = getRequest('r1')
    expect(r?.method).toBe('GET') // preserved
    expect(r?.status).toBe(200) // added
    expect(r?.mimeType).toBe('application/json')
  })
})

describe('listRequests', () => {
  beforeEach(() => {
    upsertRequest({ requestId: 'r1', url: 'https://a.com/1', host: 'a.com', method: 'GET', resourceType: 'XHR', startedAt: 100 })
    upsertRequest({ requestId: 'r2', url: 'https://b.com/2', host: 'b.com', method: 'POST', resourceType: 'Fetch', startedAt: 200 })
    upsertRequest({ requestId: 'r3', url: 'https://a.com/3', host: 'a.com', method: 'GET', resourceType: 'Script', startedAt: 300 })
  })

  it('returns newest-first', () => {
    expect(listRequests().map((r) => r.requestId)).toEqual(['r3', 'r2', 'r1'])
  })

  it('respects the limit', () => {
    expect(listRequests({ limit: 2 }).map((r) => r.requestId)).toEqual(['r3', 'r2'])
  })

  it('filters by host substring', () => {
    expect(listRequests({ host: 'a.com' }).map((r) => r.requestId)).toEqual(['r3', 'r1'])
  })

  it('filters by method (case-insensitive)', () => {
    expect(listRequests({ methodOrType: 'post' }).map((r) => r.requestId)).toEqual(['r2'])
  })

  it('filters by resourceType', () => {
    expect(listRequests({ methodOrType: 'script' }).map((r) => r.requestId)).toEqual(['r3'])
  })

  it('filters by since timestamp', () => {
    expect(listRequests({ since: 250 }).map((r) => r.requestId)).toEqual(['r3'])
  })
})

describe('response body cap', () => {
  const MAX = 8 * 1024 * 1024

  it('truncates and flags bodies above the byte ceiling', () => {
    upsertRequest({ requestId: 'big', url: 'https://x/b.js', host: 'x', responseBody: 'a'.repeat(MAX + 100) })
    const r = getRequest('big')
    expect(r?.responseBody?.length).toBe(MAX)
    expect(r?.responseBodyTruncated).toBe(true)
  })

  it('leaves normal bodies untouched', () => {
    upsertRequest({ requestId: 'ok', url: 'https://x/s.js', host: 'x', responseBody: 'hello' })
    const r = getRequest('ok')
    expect(r?.responseBody).toBe('hello')
    expect(r?.responseBodyTruncated).toBeUndefined()
  })
})

describe('eviction', () => {
  it('drops the oldest entries past MAX_ENTRIES (500)', () => {
    for (let i = 0; i < 510; i++) {
      upsertRequest({ requestId: `e${i}`, url: `https://x/${i}`, host: 'x', startedAt: i })
    }
    expect(getRequest('e0')).toBeUndefined() // evicted
    expect(getRequest('e9')).toBeUndefined() // evicted
    expect(getRequest('e10')).toBeDefined() // first survivor
    expect(getRequest('e509')).toBeDefined()
    expect(listRequests({ limit: 1000 }).length).toBe(500)
  })
})

describe('websocket frames', () => {
  it('appends frames per requestId and filters by since', () => {
    appendWsFrame('ws1', { direction: 'sent', opcode: 1, payloadData: 'a', timestamp: 10 })
    appendWsFrame('ws1', { direction: 'received', opcode: 1, payloadData: 'b', timestamp: 20 })
    expect(getWsFrames('ws1')).toHaveLength(2)
    expect(getWsFrames('ws1', 15).map((f) => f.payloadData)).toEqual(['b'])
    expect(getWsFrames('missing')).toEqual([])
  })

  it('caps frames per request so a long-lived socket cannot grow unbounded', () => {
    for (let i = 0; i < 2100; i++) {
      appendWsFrame('wsCap', { direction: 'received', opcode: 1, payloadData: `f${i}`, timestamp: i })
    }
    const frames = getWsFrames('wsCap')
    expect(frames.length).toBe(2000) // MAX_WS_FRAMES_PER_REQUEST
    expect(frames[0].payloadData).toBe('f100') // oldest 100 dropped
    expect(frames.at(-1)?.payloadData).toBe('f2099')
  })

  it('drops frames when their request is evicted from the ring buffer', () => {
    upsertRequest({ requestId: 'wsEvict', url: 'wss://x', host: 'x', resourceType: 'WebSocket', startedAt: 0 })
    appendWsFrame('wsEvict', { direction: 'sent', opcode: 1, payloadData: 'p', timestamp: 1 })
    expect(getWsFrames('wsEvict')).toHaveLength(1)
    // Push the WS request out of the 500-entry window.
    for (let i = 0; i < 500; i++) {
      upsertRequest({ requestId: `pad${i}`, url: `https://x/${i}`, host: 'x', startedAt: i + 1 })
    }
    expect(getRequest('wsEvict')).toBeUndefined() // evicted
    expect(getWsFrames('wsEvict')).toEqual([]) // frames freed, no leak
  })
})

describe('console logs', () => {
  it('appends and filters by since', () => {
    appendConsole({ ts: 10, type: 'log', text: 'one' })
    appendConsole({ ts: 20, type: 'warn', text: 'two' })
    expect(getConsoleSince().map((e) => e.text)).toEqual(['one', 'two'])
    expect(getConsoleSince(15).map((e) => e.text)).toEqual(['two'])
  })

  it('caps the ring buffer at MAX_CONSOLE (1000)', () => {
    for (let i = 0; i < 1050; i++) appendConsole({ ts: i, type: 'log', text: `m${i}` })
    const all = getConsoleSince()
    expect(all).toHaveLength(1000)
    expect(all[0].text).toBe('m50') // oldest 50 dropped
  })
})

describe('runtime exceptions', () => {
  it('appends and caps at MAX_EXCEPTIONS (200)', () => {
    for (let i = 0; i < 250; i++) appendException({ ts: i, text: `boom${i}` })
    const all = getExceptions()
    // Cap is absolute regardless of prior appends in earlier tests.
    expect(all).toHaveLength(200)
    expect(all.at(-1)?.text).toBe('boom249')
  })
})
