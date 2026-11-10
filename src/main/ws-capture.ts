// WebSocket 송신 공유 로직 — MCP 도구와 renderer IPC 양쪽이 쓴다.
// 접근 방식: 페이지에 WebSocket 생성자 래퍼를 심어 페이지가 연 소켓 자체를 통해
// 프레임을 보낸다 (인증/세션이 소켓에 묶여 있는 게 온라인 three.js의 일반적 패턴).

import { getActiveTarget, type AttachedTarget } from './chrome-cdp'
import { listOopifSessions } from './mcp/oopif'
import { listRequests } from './traffic-store'

export type WsSendResult = { ok: true; message: string; reconnected?: boolean } | { ok: false; error: string }

// 재부착 시 중복 등록 방지 — target webContentsId → scriptIdentifier
const wsCaptureScriptIds = new Map<number, string>()

export const WS_CAPTURE_SCRIPT = `(() => {
  if (window.__revWSCapture) return;
  const list = [];
  window.__revWSCapture = list;
  const OrigWS = window.WebSocket;
  function Wrapped(url, protocols) {
    const ws = protocols == null ? new OrigWS(url) : new OrigWS(url, protocols);
    const entry = { id: list.length, url: String(url), ws, openedAt: Date.now() };
    list.push(entry);
    return ws;
  }
  Wrapped.prototype = OrigWS.prototype;
  Wrapped.CONNECTING = OrigWS.CONNECTING;
  Wrapped.OPEN = OrigWS.OPEN;
  Wrapped.CLOSING = OrigWS.CLOSING;
  Wrapped.CLOSED = OrigWS.CLOSED;
  window.WebSocket = Wrapped;
})();`

/** Pure: ws_send이 페이지에서 실행할 표현식을 만든다 (텍스트/바이너리 경로). */
export function buildSendExpression(captureId: number, payload: string, asBinary: boolean): string {
  return asBinary
    ? `(() => {
        const e = (window.__revWSCapture || [])[${captureId}];
        if (!e) return 'no-such-capture';
        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
        const raw = atob(${JSON.stringify(payload)});
        const buf = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i);
        e.ws.send(buf);
        return 'ok';
      })()`
    : `(() => {
        const e = (window.__revWSCapture || [])[${captureId}];
        if (!e) return 'no-such-capture';
        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
        e.ws.send(${JSON.stringify(payload)});
        return 'ok';
      })()`
}

function activeTarget(): AttachedTarget | { error: string } {
  const target = getActiveTarget()
  return target ?? { error: 'no active browser target' }
}

export async function installWsCapture(): Promise<WsSendResult> {
  const t = activeTarget()
  if ('error' in t) return { ok: false, error: t.error }
  try {
    // 이전에 등록한 스크립트가 있으면 제거 (재부착 누수 방지)
    const wcId = t.wc.id
    const prevId = wsCaptureScriptIds.get(wcId)
    if (prevId) {
      await t.dbg
        .sendCommand('Page.removeScriptToEvaluateOnNewDocument', { identifier: prevId })
        .catch(() => {})
      wsCaptureScriptIds.delete(wcId)
    }
    const res = (await t.dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
      source: WS_CAPTURE_SCRIPT
    })) as { identifier: string }
    wsCaptureScriptIds.set(wcId, res.identifier)
    // 현재 문서에도 설치를 시도한다 (이미 있거나 기존 소켓이 있으면 no-op).
    await t.dbg.sendCommand('Runtime.evaluate', { expression: WS_CAPTURE_SCRIPT })
    return { ok: true, message: 'installed' }
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }
}

export async function sendWsFrame(
  captureId: number,
  payload: string,
  asBinary: boolean
): Promise<WsSendResult> {
  const t = activeTarget()
  if ('error' in t) return { ok: false, error: t.error }
  try {
    const r = (await t.dbg.sendCommand('Runtime.evaluate', {
      expression: buildSendExpression(captureId, payload, asBinary),
      returnByValue: true
    })) as { result: { value?: string } }
    const v = r.result.value
    if (v === 'ok') return { ok: true, message: `sent to ws#${captureId}` }
    return { ok: false, error: `ws_send failed: ${v}` }
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }
}

// ── Live sockets straight from the page heap ─────────────────────────────
// Runtime.queryObjects(WebSocket.prototype) returns every WebSocket the page
// already holds, so sending needs no constructor hook and no reload — the
// socket a page opened at load (game, chat widget) is reachable as-is.
// Each cross-site iframe runs in its own process, so its session is searched too.

export interface PageSocket {
  url: string
  readyState: number
  /** 'page', or the URL of the cross-site frame that owns the socket. */
  frame: string
}

/** Runs in the page with `this` = every live WebSocket. */
export const HEAP_LIST_FN = `function () { return this.map((w) => ({ url: w.url, readyState: w.readyState })) }`

/** Runs in the page with `this` = every live WebSocket; sends on the newest open one with this URL. */
export const HEAP_SEND_FN = `function (url, payload, asBinary) {
  const open = this.filter((w) => w.url === url && w.readyState === 1)
  const w = open[open.length - 1]
  if (!w) return this.some((x) => x.url === url) ? 'not-open' : 'no-socket'
  if (asBinary) {
    const raw = atob(payload)
    const buf = new Uint8Array(raw.length)
    for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i)
    w.send(buf)
  } else w.send(payload)
  return 'ok'
}`

const HEAP_GROUP = 'rever-ws'

async function onHeapSockets<T>(t: AttachedTarget, sessionId: string | undefined, fn: string, args: unknown[]): Promise<T> {
  const cmd = (method: string, params: object): Promise<any> => t.dbg.sendCommand(method, params, sessionId)
  try {
    const proto = await cmd('Runtime.evaluate', { expression: 'WebSocket.prototype', objectGroup: HEAP_GROUP })
    const { objects } = await cmd('Runtime.queryObjects', { prototypeObjectId: proto.result.objectId, objectGroup: HEAP_GROUP })
    const r = await cmd('Runtime.callFunctionOn', {
      objectId: objects.objectId,
      functionDeclaration: fn,
      arguments: args.map((value) => ({ value })),
      returnByValue: true
    })
    return r.result.value as T
  } finally {
    await cmd('Runtime.releaseObjectGroup', { objectGroup: HEAP_GROUP }).catch(() => {})
  }
}

function frameSessions(t: AttachedTarget): Array<{ sessionId?: string; frame: string }> {
  return [{ frame: 'page' }, ...listOopifSessions(t.wc.id).map((o) => ({ sessionId: o.sessionId, frame: o.url }))]
}

export async function listPageSockets(): Promise<PageSocket[] | { error: string }> {
  const t = activeTarget()
  if ('error' in t) return t
  const out: PageSocket[] = []
  for (const s of frameSessions(t)) {
    // A frame that is navigating or already gone just has nothing to list.
    const found = await onHeapSockets<Array<{ url: string; readyState: number }>>(t, s.sessionId, HEAP_LIST_FN, []).catch(() => [])
    out.push(...found.map((w) => ({ ...w, frame: s.frame })))
  }
  return out
}

// ── Reconnect a socket that has closed ───────────────────────────────────
// The page opens the replacement itself, so cookies and Origin are the page's
// own. An open socket is never garbage-collected, so the heap search above
// finds it afterwards like any socket the page opened.

/** Pure: what a reconnect has to repeat from the original handshake request. */
export function handshakeInfo(headers: Record<string, string> | undefined): { protocols: string[]; origin?: string } {
  const get = (name: string): string | undefined => Object.entries(headers ?? {}).find(([k]) => k.toLowerCase() === name)?.[1]
  return {
    protocols: (get('sec-websocket-protocol') ?? '').split(',').map((s) => s.trim()).filter(Boolean),
    origin: get('origin')
  }
}

/** Runs in the page: opens a new socket and resolves 'ok' once it is open, else the reason it is not. */
export const RECONNECT_FN = `function (url, protocols) {
  return new Promise((resolve) => {
    let ws
    try { ws = protocols.length ? new WebSocket(url, protocols) : new WebSocket(url) } catch (e) { return resolve(String(e && e.message || e)) }
    const timer = setTimeout(() => { ws.close(); resolve('no answer from the server within 5s') }, 5000)
    ws.onopen = () => { clearTimeout(timer); resolve('ok') }
    ws.onclose = (e) => { clearTimeout(timer); resolve('the server refused the connection (close code ' + e.code + (e.reason ? ', ' + e.reason : '') + ')') }
  })
}`

async function reconnectSocket(t: AttachedTarget, url: string): Promise<string> {
  // The newest capture of this URL says which subprotocols to offer and which frame opened it.
  const prev = listRequests({ methodOrType: 'WebSocket', limit: 200 }).find((r) => r.url === url)
  const { protocols, origin } = handshakeInfo(prev?.requestHeaders)
  // Reopen it in the cross-site frame that sent that Origin, else in the page.
  // The frame is asked for its origin: the session's recorded URL is empty when it attaches before navigating.
  let sessionId: string | undefined
  for (const o of origin ? listOopifSessions(t.wc.id) : []) {
    const r = (await t.dbg.sendCommand('Runtime.evaluate', { expression: 'origin', returnByValue: true }, o.sessionId).catch(() => null)) as {
      result?: { value?: string }
    } | null
    if (r?.result?.value === origin) {
      sessionId = o.sessionId
      break
    }
  }
  try {
    const r = (await t.dbg.sendCommand(
      'Runtime.evaluate',
      { expression: `(${RECONNECT_FN})(${JSON.stringify(url)}, ${JSON.stringify(protocols)})`, awaitPromise: true, returnByValue: true },
      sessionId
    )) as { result?: { value?: string }; exceptionDetails?: { text?: string } }
    return r.result?.value ?? r.exceptionDetails?.text ?? 'the page did not answer'
  } catch (e) {
    return e instanceof Error ? e.message : String(e)
  }
}

/**
 * Send on the page's own live socket for `url` (page first, then cross-site frames).
 * With `reconnect`, a socket that has closed is reopened from the page first.
 */
export async function sendWsByUrl(url: string, payload: string, asBinary: boolean, reconnect = false): Promise<WsSendResult> {
  const t = activeTarget()
  if ('error' in t) return { ok: false, error: t.error }
  let notOpen = false
  const sendLive = async (): Promise<string | undefined> => {
    for (const s of frameSessions(t)) {
      const v = await onHeapSockets<string>(t, s.sessionId, HEAP_SEND_FN, [url, payload, asBinary]).catch(() => 'no-socket')
      if (v === 'ok') return `sent on ${url}${s.sessionId ? ` (frame ${s.frame})` : ''}`
      if (v === 'not-open') notOpen = true
    }
    return undefined
  }
  const sent = await sendLive()
  if (sent) return { ok: true, message: sent }
  if (reconnect) {
    const opened = await reconnectSocket(t, url)
    if (opened !== 'ok') return { ok: false, error: `could not reconnect ${url}: ${opened}` }
    const resent = await sendLive()
    if (resent) return { ok: true, message: `reconnected, ${resent}`, reconnected: true }
  }
  return { ok: false, error: notOpen ? `the socket for ${url} is no longer open` : `no live socket for ${url} in the page or its frames` }
}
