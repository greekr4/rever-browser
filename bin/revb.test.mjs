import { describe, expect, it } from 'vitest'

import { fetchRest, parsePage, parseTruncated, pickRpcResponse, toValue, userDataDir } from './revb.mjs'

describe('pickRpcResponse', () => {
  it('SSE 스트림에서 앞선 알림을 건너뛰고 id가 맞는 응답을 고른다', () => {
    const raw =
      'event: message\ndata: {"jsonrpc":"2.0","method":"notifications/progress","params":{}}\n\n' +
      'event: message\ndata: {"jsonrpc":"2.0","id":7,"result":{"ok":1}}\n\n'
    expect(pickRpcResponse(raw, 7)).toEqual({ jsonrpc: '2.0', id: 7, result: { ok: 1 } })
  })

  it('id가 다른 응답만 있으면 null을 돌려준다', () => {
    expect(pickRpcResponse('data: {"jsonrpc":"2.0","id":1,"result":{}}\n\n', 2)).toBeNull()
  })

  it('여러 줄 data를 이어 붙여 하나의 JSON으로 파싱한다', () => {
    const raw = 'data: {"jsonrpc":"2.0",\ndata: "id":3,"result":{}}\n\n'
    expect(pickRpcResponse(raw, 3)?.id).toBe(3)
  })

  it('SSE가 아닌 일반 JSON 본문도 처리한다', () => {
    expect(pickRpcResponse('{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"x"}}', 1)?.error.code).toBe(-32601)
  })

  it('202 빈 본문이면 null을 돌려준다', () => {
    expect(pickRpcResponse('', 1)).toBeNull()
  })
})

describe('toValue', () => {
  it('JSON 텍스트는 객체로 바꾼다', () => {
    expect(toValue('[{"a":1}]')).toEqual([{ a: 1 }])
    expect(toValue('  {"x":true}\n')).toEqual({ x: true })
  })

  it('JSON이 아닌 텍스트는 문자열 그대로 둔다', () => {
    expect(toValue('- button "OK" [ref=r1]')).toBe('- button "OK" [ref=r1]')
    expect(toValue('{ not json')).toBe('{ not json')
  })
})

describe('절단 페이징', () => {
  const footer = (shown, total, id) =>
    `\n\n[truncated: showing ${shown} of ${total} chars. Call tool_result_more with resultId="${id}", offset=${shown} for the rest.]`

  it('절단 푸터를 떼어 resultId와 offset을 읽는다', () => {
    expect(parseTruncated('abc' + footer(3, 10, 'res_4'))).toEqual({ head: 'abc', total: 10, resultId: 'res_4', offset: 3 })
    expect(parseTruncated('no footer here')).toBeNull()
  })

  it('tool_result_more 페이지 푸터를 해석한다', () => {
    expect(parsePage('def\n\n[more: call tool_result_more resultId="res_4", offset=6 (4 chars left)]')).toEqual({ slice: 'def', nextOffset: 6 })
    expect(parsePage('ghij\n\n[end of result: 10 chars total]')).toEqual({ slice: 'ghij', nextOffset: null })
  })

  it('푸터 없이 전체 텍스트를 이어 받는다', async () => {
    const full = '{"items":[1,2,3,4,5,6,7,8,9]}'
    const more = async (id, offset) => {
      expect(id).toBe('res_1')
      const end = Math.min(offset + 8, full.length)
      return end < full.length
        ? `${full.slice(offset, end)}\n\n[more: call tool_result_more resultId="res_1", offset=${end} (${full.length - end} chars left)]`
        : `${full.slice(offset)}\n\n[end of result: ${full.length} chars total]`
    }
    const out = await fetchRest(full.slice(0, 5) + footer(5, full.length, 'res_1'), more)
    expect(out).toBe(full)
    expect(toValue(out)).toEqual({ items: [1, 2, 3, 4, 5, 6, 7, 8, 9] })
  })

  it('페이지 상한에 닿으면 남은 위치를 담은 푸터를 남긴다', async () => {
    const more = async (_id, offset) =>
      `x\n\n[more: call tool_result_more resultId="res_9", offset=${offset + 1} (99 chars left)]`
    const out = await fetchRest('a' + footer(1, 100, 'res_9'), more, 2)
    expect(out).toMatch(/^axx\n\n\[truncated: revb stopped after 2 pages at 3 of 100 chars/)
  })

  it('절단되지 않은 결과는 그대로 돌려준다', async () => {
    expect(await fetchRest('short', async () => '')).toBe('short')
  })
})

describe('userDataDir', () => {
  it('REVER_USER_DATA_DIR가 있으면 그것을 쓴다', () => {
    expect(userDataDir({ REVER_USER_DATA_DIR: '/tmp/e2e' }, 'darwin', '/h')).toBe('/tmp/e2e')
  })

  it('OS별 appData 아래 rever-browser를 가리킨다', () => {
    expect(userDataDir({}, 'darwin', '/h')).toBe('/h/Library/Application Support/rever-browser')
    expect(userDataDir({ APPDATA: 'C:\\AD' }, 'win32', '/h')).toMatch(/C:\\AD.rever-browser$/)
    expect(userDataDir({}, 'linux', '/h')).toBe('/h/.config/rever-browser')
  })
})
