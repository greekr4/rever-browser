import { describe, expect, it } from 'vitest'

import {
  EXTRACT_NOTE_NONE,
  RAW_PREVIEW_CHARS,
  WRITES_DISABLED,
  attachExtract,
  buildExtractExpression,
  capturedFlagValues,
  cliRunExample,
  compileCli,
  describeCli,
  exportCurl,
  parseExtract,
  presentRawBody,
  runJsonBody,
  runGate,
  scrubExtractItems,
  type CapturedCall,
  type CliCommand,
  type CliData
} from './cli-spec'

const UUID = '550e8400-e29b-41d4-a716-446655440000'
const UUID_2 = '550e8400-e29b-41d4-a716-446655440001'

function call(partial: Partial<CapturedCall> & Pick<CapturedCall, 'requestId' | 'url'>): CapturedCall {
  return { method: 'GET', ...partial }
}

describe('cli-spec', () => {
  it('같은 UUID 경로의 두 호출은 명령 하나로 합쳐지고 경로는 {id0}이 된다', () => {
    const data = compileCli([
      call({
        requestId: 'a',
        method: 'GET',
        url: `https://shop.example/api/orders/${UUID}`,
        status: 200
      }),
      call({
        requestId: 'b',
        method: 'GET',
        url: `https://shop.example/api/orders/${UUID_2}`,
        status: 200
      })
    ])
    expect(data.commands).toHaveLength(1)
    expect(data.commands[0]?.path).toBe('/api/orders/{id0}')
    expect(data.commands[0]?.evidenceRequestIds).toEqual(['a', 'b'])
    expect(data.writes).toBe(false)
  })

  it('js와 woff2는 명령이 되지 않는다', () => {
    const data = compileCli([
      call({ requestId: 'js', url: 'https://shop.example/assets/app.js', resourceType: 'Script' }),
      call({ requestId: 'font', url: 'https://shop.example/font.woff2', resourceType: 'Font' }),
      call({ requestId: 'api', url: 'https://shop.example/api/health', resourceType: 'XHR', status: 200 })
    ])
    expect(data.commands.map((command) => command.path)).toEqual(['/api/health'])
  })

  it('q와 JSON title은 플래그가 되고 csrf_token과 authorization은 빠진다', () => {
    const data = compileCli([
      call({
        requestId: 'post',
        method: 'POST',
        url: 'https://shop.example/api/items?q=hat',
        resourceType: 'Fetch',
        status: 201,
        mimeType: 'application/json',
        requestHeaders: {
          authorization: 'Bearer secret-value',
          'x-csrf-token': 'secret-value'
        },
        requestPostData: JSON.stringify({
          title: 'Hat',
          csrf_token: 'secret-value',
          authorization: 'secret-value',
          pass: 'hunter2'
        }),
        responseBody: '{"ok":true}'
      })
    ])
    const command = data.commands[0]
    expect(command?.query.q).toEqual({ kind: 'string', required: false })
    expect(command?.body?.json?.title).toEqual({ kind: 'string', required: false })
    expect(command?.body?.json?.pass).toEqual({ kind: 'string', required: false })
    expect(command?.body?.json?.csrf_token).toBeUndefined()
    expect(command?.body?.json?.authorization).toBeUndefined()
    expect(command?.sessionHeaders).toEqual(['x-csrf-token'])
    const exported = exportCurl(data)
    expect(exported).toContain('$title')
    expect(exported).toContain('$pass')
    expect(exported).not.toContain('hunter2')
    expect(exported).not.toContain('secret-value')
    expect(exported).not.toContain('Cookie')
    expect(JSON.stringify(data)).not.toContain('hunter2')
    expect(JSON.stringify(data)).not.toContain('secret-value')
  })

  it('GET /users/delete는 write다', () => {
    const data = compileCli([
      call({ requestId: 'del', method: 'GET', url: 'https://shop.example/users/delete', status: 200 })
    ])
    expect(data.commands[0]?.effect).toBe('write')
  })

  it('names로 requestId별 명령 이름을 지정한다', () => {
    const data = compileCli(
      [call({ requestId: 's1', url: 'https://en.wikipedia.org/w/rest.php/v1/search/title?q=x&limit=10' })],
      { names: { s1: 'Search Titles!' } }
    )
    expect(data.commands[0]?.name).toBe('search-titles')
  })

  it('지정한 이름은 effect 판정을 바꾸지 않는다', () => {
    const data = compileCli(
      [call({ requestId: 'del', method: 'GET', url: 'https://shop.example/users/delete', status: 200 })],
      { names: { del: 'search' } }
    )
    expect(data.commands[0]?.name).toBe('search')
    expect(data.commands[0]?.effect).toBe('write')
  })

  it('지정한 이름이 겹치거나 비면 자동 이름 규칙을 따른다', () => {
    const data = compileCli(
      [
        call({ requestId: 'a', url: 'https://shop.example/api/items' }),
        call({ requestId: 'b', url: 'https://shop.example/api/orders' }),
        call({ requestId: 'c', url: 'https://shop.example/api/cart' })
      ],
      { names: { a: 'list', b: 'list', c: '!!!' } }
    )
    expect(data.commands.map((command) => command.name)).toEqual(['list', 'list-x', 'get-api-cart'])
  })

  it('curl 내보내기는 클라이언트를 밝히는 User-Agent 자리를 둔다', () => {
    const data = compileCli([call({ requestId: 's1', url: 'https://en.wikipedia.org/w/rest.php/v1/search/title?q=x' })])
    const exported = exportCurl(data)
    expect(exported).toMatch(/^# UA .*User-Agent/m)
    expect(exported).toContain('curl -sS -A "$UA" -X GET')
  })

  it('실행 결과가 온전한 JSON 본문이면 뷰어에 넘길 본문을 돌려준다', () => {
    expect(runJsonBody({ status: 200, body: '{"pages":[{"title":"Web browser"}]}' })).toBe(
      '{"pages":[{"title":"Web browser"}]}'
    )
    expect(runJsonBody({ status: 200, body: ' [1,2] ' })).toBe(' [1,2] ')
  })

  it('잘린 본문, HTML, 추출 결과는 JSON 뷰어로 보내지 않는다', () => {
    expect(runJsonBody({ status: 200, body: '{"pages":[', bodyCut: true })).toBeNull()
    expect(runJsonBody({ status: 200, body: '<html><body>hi</body></html>' })).toBeNull()
    expect(runJsonBody({ status: 200, body: '{"a":1}', items: [{ title: 'x' }] })).toBeNull()
    expect(runJsonBody({ status: 200, body: '{not json}' })).toBeNull()
    expect(runJsonBody({ status: 200 })).toBeNull()
  })

  it('빈 requestId는 증거가 되지 않는다', () => {
    const data = compileCli([
      call({ requestId: '', url: 'https://shop.example/api/health' }),
      call({ requestId: '   ', url: 'https://shop.example/api/health' })
    ])
    expect(data.commands).toEqual([])
  })

  it('writes가 꺼져 있으면 write 실행은 요청을 만들지 않고 막는다', () => {
    expect(runGate('write', false)).toBe(WRITES_DISABLED)
    expect(runGate('unknown', false)).toBe(WRITES_DISABLED)
    expect(runGate('read', false)).toBeNull()
    expect(runGate('write', true)).toBeNull()
  })

  it('extract는 셀렉터와 필드로 저장되고 한계는 1에서 20이다', () => {
    const parsed = parseExtract({
      selector: 'a[data-heatmap-target=".nblg"]',
      fields: [
        { name: 'title', selector: '.headline' },
        { name: 'link', attr: 'href' },
        { name: 'label', attr: 'text' }
      ]
    })
    expect(parsed.ok).toBe(true)
    if (!parsed.ok || !parsed.extract) throw new Error('extract')
    expect(parsed.extract.limit).toBe(5)
    expect(parsed.extract.fields[1]).toEqual({ name: 'link', attr: 'href' })
    expect(parsed.extract.fields[2]).toEqual({ name: 'label' })
    expect(parseExtract({ selector: 'a', limit: 20, fields: [{ name: 'title' }] }).ok).toBe(true)
    expect(parseExtract({ selector: 'a', limit: 21, fields: [{ name: 'title' }] }).ok).toBe(false)
    expect(parseExtract({ selector: 'a', limit: 0, fields: [{ name: 'title' }] }).ok).toBe(false)
    expect(parseExtract({ fields: [{ name: 'title' }] }).ok).toBe(false)
    expect(parseExtract({ selector: 'a\nb', fields: [{ name: 'title' }] }).ok).toBe(false)
    expect(parseExtract({ selector: 'a', fields: [{ name: 'title' }, { name: 'title' }] }).ok).toBe(false)
  })

  it('깨진 extract는 명령에서 빠지고 설명과 curl에는 셀렉터만 남는다', () => {
    const command: CliCommand = {
      name: 'get-search',
      desc: 'search',
      method: 'GET',
      path: '/search',
      effect: 'read',
      evidenceRequestIds: ['r1'],
      params: {},
      query: { query: { kind: 'string', required: false } },
      sessionHeaders: [],
      responsePointers: [],
      extract: { selector: 'article h1', limit: 5, fields: [{ name: 'title' }, { name: 'link', attr: 'href' }] }
    }
    const data: CliData = {
      site: 'example',
      title: 'example',
      baseUrl: 'https://example.test',
      commands: [command],
      writes: false
    }
    const described = describeCli(data, 'get-search')
    expect(described).toContain('extract: article h1  limit 5')
    expect(described).toContain('title text')
    expect(described).toContain('link attr href')
    const exported = exportCurl(data)
    expect(exported).toContain('# selector: article h1')
    expect(exported).toContain('# reads 5: title, link')
    expect(exported).not.toContain('Cookie')
    expect(attachExtract({ ...command, extract: { selector: '', limit: 5, fields: [] } }).extract).toBeUndefined()
    expect(attachExtract(command).extract?.selector).toBe('article h1')
  })

  it('호출 예시는 id·명령·플래그만 담고 캡처 값에서 시크릿은 빼낸다', () => {
    const command: CliCommand = {
      name: 'get-searchnaver',
      desc: 'search',
      method: 'GET',
      path: '/search.naver',
      effect: 'read',
      evidenceRequestIds: ['r1'],
      params: {},
      query: {
        ssc: { kind: 'string', required: false },
        where: { kind: 'string', required: false },
        query: { kind: 'string', required: false },
        token: { kind: 'string', required: false }
      },
      sessionHeaders: [],
      responsePointers: []
    }
    const captured = capturedFlagValues(command, [
      {
        url: 'https://search.naver.com/search.naver?ssc=tab.blog.all&where=blog&query=%EC%A0%9C%EC%A3%BC%EB%8F%84+%EB%A7%9B%EC%A7%91&token=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.signaturevalue',
        requestHeaders: { cookie: 'NID_SES=secret' }
      }
    ])
    expect(captured).toEqual({ ssc: 'tab.blog.all', where: 'blog', query: '제주도 맛집' })
    expect(captured).not.toHaveProperty('token')
    const example = JSON.parse(cliRunExample('wmvew9vxip06nak', command, captured)) as {
      id: string
      command: string
      inputs: Record<string, string>
    }
    expect(example).toEqual({
      id: 'wmvew9vxip06nak',
      command: 'get-searchnaver',
      inputs: { ssc: 'tab.blog.all', where: 'blog', query: '제주도 맛집', token: '' }
    })
    expect(JSON.stringify(example)).not.toMatch(/cookie/i)
    const blank = JSON.parse(cliRunExample('wmvew9vxip06nak', command, {})) as { inputs: Record<string, string> }
    expect(blank.inputs.query).toBe('')
  })

  it('HTML 본문은 앞부분만 남기고 JSON 본문은 그대로 둔다', () => {
    const html = `<!doctype html><html><body>${'x'.repeat(RAW_PREVIEW_CHARS)}</body></html>`
    const cut = presentRawBody(html, true)
    expect(cut.bodyCut).toBe(true)
    expect(cut.body.length).toBe(RAW_PREVIEW_CHARS)
    const json = '{"name":"rever"}'
    expect(presentRawBody(json, false)).toEqual({ body: json, bodyCut: false })
    expect(presentRawBody(`${json}${'y'.repeat(2_000)}`, false).bodyCut).toBe(false)
  })

  it('extract 값의 시크릿은 지우고 셀렉터는 스크립트에 문자열로만 들어간다', () => {
    const items = scrubExtractItems([
      { title: 'Bearer abcdefghijklmnop', token: 'visible-token' },
      { link: 'https://blog.example/1' }
    ])
    expect(items[0]?.title).toBe('Bearer ****')
    expect(items[0]?.token).toBe('****')
    expect(items[1]?.link).toBe('https://blog.example/1')
    const ordered = scrubExtractItems([{ link: 'https://blog.example/1', title: '제목' }], [
      { name: 'title' },
      { name: 'link', attr: 'href' }
    ])
    expect(Object.keys(ordered[0] ?? {})).toEqual(['title', 'link'])
    const selector = `a[href="x"]</script>`
    const expr = buildExtractExpression({
      url: 'https://search.example/search',
      method: 'GET',
      headers: {},
      extract: { selector, limit: 5, fields: [{ name: 'title', selector: '.h' }, { name: 'link', attr: 'href' }] }
    })
    expect(expr).toContain(JSON.stringify(selector))
    expect(expr).toContain("credentials: 'include'")
    expect(expr).toContain(EXTRACT_NOTE_NONE)
    expect(expr).not.toContain('Cookie')
    expect(expr).not.toContain('eval(')
  })
})
