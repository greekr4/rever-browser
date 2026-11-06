// 캡처된 프레임의 연결(requestId)을 페이지 소켓 캡처(captureId)로 이어준다.
// 이유: 프레임은 CDP가 본 연결 URL로 식별되고, 송신은 페이지 소켓 캡처로만
// 가능하다. 정확히 일치하면 그대로 쓰고, 아니면 쿼리스트링을 무시하고 비교한다
// (재연결 시 토큰이 바뀌는 게 온라인 게임의 일반적인 패턴).

export interface WsCaptureLike {
  id: number
  url: string
  readyState: number
}

function stripQuery(url: string): string {
  const i = url.indexOf('?')
  return i === -1 ? url : url.slice(0, i)
}

/** 프레임의 연결 URL로 보낼 수 있는 열린 페이지 소켓을 찾는다. 없으면 null. */
export function matchCaptureForUrl<T extends WsCaptureLike>(captures: T[], url: string): T | null {
  const open = captures.filter((c) => c.readyState === 1)
  return open.find((c) => c.url === url) ?? open.find((c) => stripQuery(c.url) === stripQuery(url)) ?? null
}
