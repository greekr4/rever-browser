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
  getExceptions,
  mergeExtraResponseHeaders,
  takePendingExtraResponseHeaders,
  discardPendingExtraResponseHeaders,
  importRequests,
  clearImports
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

  // `before`/`beforeId` are the pagination cursor pair: entries strictly
  // older than the boundary entry. `before` alone stays inclusive on the
  // boundary ms — HAR entries carry no requestId, so a caller paging by
  // timestamp alone can't name the boundary and would lose same-ms
  // siblings. The emitted _nextBefore/_nextBeforeId pair is what makes
  // each page advance.
  it('before alone keeps the boundary-ms entries (inclusive fallback)', () => {
    expect(listRequests({ before: 300 }).map((r) => r.requestId)).toEqual(['r3', 'r2', 'r1'])
  })

  it('before+beforeId excludes the boundary entry so the page advances', () => {
    expect(listRequests({ before: 300, beforeId: 'r3' }).map((r) => r.requestId)).toEqual([
      'r2',
      'r1'
    ])
  })

  it('before+beforeId keeps same-ms siblings older than the boundary', () => {
    upsertRequest({ requestId: 'r4', url: 'https://c.com/4', host: 'c.com', method: 'GET', resourceType: 'XHR', startedAt: 300 })
    // r3 and r4 share ms 300; r4 was inserted later (newer). Boundary=r4
    // keeps the older sibling r3…
    expect(listRequests({ before: 300, beforeId: 'r4' }).map((r) => r.requestId).sort()).toEqual([
      'r1',
      'r2',
      'r3'
    ])
    // …and boundary=r3 excludes the newer same-ms sibling r4.
    expect(listRequests({ before: 300, beforeId: 'r3' }).map((r) => r.requestId)).toEqual([
      'r2',
      'r1'
    ])
  })

  it('before+beforeId at the oldest entry returns empty — pagination done', () => {
    expect(listRequests({ before: 100, beforeId: 'r1' }).map((r) => r.requestId)).toEqual([])
  })

  it('before and since together select a window', () => {
    expect(listRequests({ since: 150, before: 299 }).map((r) => r.requestId)).toEqual(['r2'])
  })

  it('before older than every entry returns empty', () => {
    expect(listRequests({ before: 50 }).map((r) => r.requestId)).toEqual([])
  })

  it('evicted beforeId falls back to excluding only the exact id', () => {
    // The boundary entry is gone from the store — same-ms siblings can't be
    // ordered against it, so they're kept and the caller dedupes.
    expect(listRequests({ before: 300, beforeId: 'gone' }).map((r) => r.requestId)).toEqual([
      'r3',
      'r2',
      'r1'
    ])
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

  it('drops all frames when traffic is cleared', () => {
    upsertRequest({ requestId: 'wsClear', url: 'wss://x', host: 'x', resourceType: 'WebSocket' })
    appendWsFrame('wsClear', { direction: 'sent', opcode: 1, payloadData: 'p', timestamp: 1 })
    clearTraffic()
    expect(getWsFrames('wsClear')).toEqual([])
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

describe('responseReceivedExtraInfo merge (Set-Cookie capture)', () => {
  // CDP Network.responseReceived.response.headers omits Set-Cookie; the raw
  // header block arrives in responseReceivedExtraInfo in either order.
  const extra = {
    'Content-Type': 'text/html',
    'Set-Cookie': 'session=abc; HttpOnly\npref=dark; Secure'
  }

  it('merges when ExtraInfo arrives before responseReceived', () => {
    upsertRequest({ requestId: 'r1', url: 'https://a.com/', host: 'a.com' })
    mergeExtraResponseHeaders('r1', extra, 200)
    // responseReceived lands: caller merges pending extra headers over the
    // event's own header map (same shape as chrome-cdp.ts / external-cdp.ts).
    const pending = takePendingExtraResponseHeaders('r1', 200)
    upsertRequest({
      requestId: 'r1',
      status: 200,
      responseHeaders: { ...{ 'Content-Type': 'text/html', Server: 'x' }, ...pending }
    })
    const h = getRequest('r1')?.responseHeaders ?? {}
    expect(h['Set-Cookie']).toBe('session=abc; HttpOnly\npref=dark; Secure')
    expect(h['Server']).toBe('x')
  })

  it('merges when ExtraInfo arrives after responseReceived', () => {
    upsertRequest({
      requestId: 'r2',
      url: 'https://a.com/',
      host: 'a.com',
      status: 200,
      responseHeaders: { 'Content-Type': 'text/html' }
    })
    mergeExtraResponseHeaders('r2', extra, 200)
    const h = getRequest('r2')?.responseHeaders ?? {}
    expect(h['Set-Cookie']).toBe('session=abc; HttpOnly\npref=dark; Secure')
    expect(h['Content-Type']).toBe('text/html')
    expect(takePendingExtraResponseHeaders('r2', 200)).toBeUndefined()
  })

  it('returns undefined for requests with no buffered extra info', () => {
    expect(takePendingExtraResponseHeaders('nope', 200)).toBeUndefined()
  })

  it('lets the raw ExtraInfo block win over the event header map', () => {
    upsertRequest({
      requestId: 'r2b',
      url: 'https://a.com/',
      host: 'a.com',
      status: 200,
      responseHeaders: { 'X-Shared': 'event-value' }
    })
    mergeExtraResponseHeaders('r2b', { 'X-Shared': 'raw-value' }, 200)
    expect(getRequest('r2b')?.responseHeaders?.['X-Shared']).toBe('raw-value')
  })

  it('drops a redirect hop ExtraInfo instead of leaking it onto the final response', () => {
    // Redirect hops share the requestId: /login 302 (Set-Cookie: hop=REDIRECT)
    // followed by /home 200 (Set-Cookie: hop=FINAL). The 302's raw block must
    // never land on the 200's stored headers.
    upsertRequest({ requestId: 'r3', url: 'https://a.com/login', host: 'a.com' })
    mergeExtraResponseHeaders('r3', { 'Set-Cookie': 'hop=REDIRECT' }, 302)
    // requestWillBeSent for the next hop carries redirectResponse → discard.
    discardPendingExtraResponseHeaders('r3')
    const pending = takePendingExtraResponseHeaders('r3', 200)
    upsertRequest({
      requestId: 'r3',
      status: 200,
      responseHeaders: { ...{ 'Content-Type': 'text/html' }, ...pending }
    })
    // The final hop's own ExtraInfo arrives after and wins.
    mergeExtraResponseHeaders('r3', { 'Set-Cookie': 'hop=FINAL' }, 200)
    expect(getRequest('r3')?.responseHeaders?.['Set-Cookie']).toBe('hop=FINAL')
  })

  it('drops a buffered redirect block whose statusCode does not match', () => {
    // A 302's ExtraInfo buffered after the redirect requestWillBeSent (or
    // without one being observed) is still rejected by the status check.
    upsertRequest({ requestId: 'r4', url: 'https://a.com/login', host: 'a.com' })
    mergeExtraResponseHeaders('r4', { 'Set-Cookie': 'hop=REDIRECT' }, 302)
    expect(takePendingExtraResponseHeaders('r4', 200)).toBeUndefined()
    // Consumed — a later take must not resurrect it.
    expect(takePendingExtraResponseHeaders('r4', 302)).toBeUndefined()
  })

  it('drops a late redirect-hop ExtraInfo arriving after the final response', () => {
    upsertRequest({
      requestId: 'r5',
      url: 'https://a.com/home',
      host: 'a.com',
      status: 200,
      responseHeaders: { 'Set-Cookie': 'hop=FINAL' }
    })
    mergeExtraResponseHeaders('r5', { 'Set-Cookie': 'hop=REDIRECT' }, 302)
    expect(getRequest('r5')?.responseHeaders?.['Set-Cookie']).toBe('hop=FINAL')
  })

  it('keeps the final response headers for same-requestId auth retries', () => {
    // HTTP auth challenges reuse the requestId without redirectResponse:
    // 401 → 200. The 200's ExtraInfo can land while the stored status is
    // still 401 — it must not be dropped as a stale redirect hop.
    upsertRequest({ requestId: 'r6', url: 'https://a.com/', host: 'a.com' })
    mergeExtraResponseHeaders('r6', { 'Set-Cookie': 'challenge=1' }, 401)
    upsertRequest({
      requestId: 'r6',
      status: 401,
      responseHeaders: {
        ...{ 'WWW-Authenticate': 'Basic' },
        ...takePendingExtraResponseHeaders('r6', 401)
      }
    })
    mergeExtraResponseHeaders('r6', { 'Set-Cookie': 'hop=FINAL' }, 200)
    upsertRequest({
      requestId: 'r6',
      status: 200,
      responseHeaders: {
        ...{ 'Content-Type': 'text/html' },
        ...takePendingExtraResponseHeaders('r6', 200)
      }
    })
    expect(getRequest('r6')?.responseHeaders?.['Set-Cookie']).toBe('hop=FINAL')
  })
})

describe('가져온 캡처 (import)', () => {
  const imp = (id: string, startedAt = 1) => ({
    requestId: id,
    url: `https://x.io/${id}`,
    host: 'x.io',
    method: 'GET',
    resourceType: 'Fetch',
    startedAt
  })

  beforeEach(() => clearImports())

  it('가져온 항목은 getRequest로 찾을 수 있다', () => {
    importRequests([imp('har:a:0')])
    expect(getRequest('har:a:0')?.url).toBe('https://x.io/har:a:0')
  })

  it('listRequests는 기본으로 실시간 캡처만, source로 가져온 것·전체를 고른다', () => {
    upsertRequest({ requestId: 'live-1', url: 'https://x.io/live', host: 'x.io', method: 'GET', resourceType: 'Fetch', startedAt: 5 })
    importRequests([imp('har:a:0')])
    expect(listRequests().map((r) => r.requestId)).not.toContain('har:a:0')
    expect(listRequests({ source: 'import' }).map((r) => r.requestId)).toEqual(['har:a:0'])
    expect(listRequests({ source: 'all' }).map((r) => r.requestId).sort()).toEqual(['har:a:0', 'live-1'])
  })

  it('가져온 항목은 5,000건을 넘으면 오래된 것부터 지운다', () => {
    importRequests(Array.from({ length: 5_001 }, (_, i) => imp(`har:b:${i}`)))
    expect(getRequest('har:b:0')).toBeUndefined()
    expect(getRequest('har:b:5000')).toBeDefined()
  })

  it('clearImports는 가져온 항목만 지운다', () => {
    upsertRequest({ requestId: 'live-2', url: 'u', host: 'x.io', method: 'GET', resourceType: 'Fetch', startedAt: 1 })
    importRequests([imp('har:c:0')])
    clearImports()
    expect(getRequest('har:c:0')).toBeUndefined()
    expect(getRequest('live-2')).toBeDefined()
  })
})
