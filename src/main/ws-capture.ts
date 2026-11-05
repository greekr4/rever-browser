// WebSocket 송신 공유 로직 — MCP 도구와 renderer IPC 양쪽이 쓴다.
// 접근 방식: 페이지에 WebSocket 생성자 래퍼를 심어 페이지가 연 소켓 자체를 통해
// 프레임을 보낸다 (인증/세션이 소켓에 묶여 있는 게 온라인 three.js의 일반적 패턴).

import { getActiveTarget, type AttachedTarget } from './chrome-cdp'

export interface WsCaptureEntry {
  id: number
  url: string
  openedAt: number
  readyState: number
}

export type WsSendResult = { ok: true; message: string } | { ok: false; error: string }

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

export async function listWsCaptures(): Promise<WsCaptureEntry[] | { error: string }> {
  const t = activeTarget()
  if ('error' in t) return t
  try {
    const r = (await t.dbg.sendCommand('Runtime.evaluate', {
      expression: `JSON.stringify((window.__revWSCapture || []).map(e => ({ id: e.id, url: e.url, openedAt: e.openedAt, readyState: e.ws.readyState })))`,
      returnByValue: true
    })) as { result: { value?: string } }
    try {
      return JSON.parse(r.result.value ?? '[]') as WsCaptureEntry[]
    } catch {
      return []
    }
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) }
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
