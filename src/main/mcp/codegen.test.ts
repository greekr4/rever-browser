import { describe, expect, it } from 'vitest'

import { buildRequestIR, generateCode, type CodegenInput } from './codegen'

const jsonPost: CodegenInput = {
  method: 'post',
  url: 'https://api.example.com/v3/order',
  requestHeaders: {
    'Content-Type': 'application/json',
    Authorization: 'Bearer supersecrettoken1234567890',
    Cookie: 'SESSIONID=abcdef1234567890; theme=dark'
  },
  requestPostData: '{"item":"x","qty":2}'
}

describe('codegen', () => {
  describe('buildRequestIR', () => {
    it('쿠키 헤더를 분리하고 메서드를 대문자화한다', () => {
      const ir = buildRequestIR(jsonPost)
      expect(ir.method).toBe('POST')
      expect(ir.headers.Cookie).toBeUndefined()
      expect(ir.cookies.SESSIONID).toBe('abcdef1234567890')
      expect(ir.cookies.theme).toBe('dark')
    })
    it('content-type로 bodyType를 판별한다', () => {
      expect(buildRequestIR(jsonPost).bodyType).toBe('json')
      expect(
        buildRequestIR({
          method: 'POST',
          url: 'u',
          requestHeaders: { 'content-type': 'application/x-www-form-urlencoded' },
          requestPostData: 'a=1&b=2'
        }).bodyType
      ).toBe('form')
    })
  })

  describe('generateCode masking', () => {
    it('기본값으로 민감 헤더·쿠키를 마스킹한다', () => {
      const py = generateCode(jsonPost, { lang: 'python' })
      expect(py).not.toContain('supersecrettoken1234567890')
      expect(py).not.toContain('abcdef1234567890')
      expect(py).toContain('Bear…7890') // masked authorization value
    })
    it('maskSecrets=false면 원문을 그대로 낸다', () => {
      const py = generateCode(jsonPost, { lang: 'python', maskSecrets: false })
      expect(py).toContain('supersecrettoken1234567890')
    })
    it('URL 쿼리의 토큰 파라미터를 마스킹한다', () => {
      const c = generateCode(
        { method: 'GET', url: 'https://x.io/a?access_token=querysecret123456789&page=2' },
        { lang: 'curl' }
      )
      expect(c).not.toContain('querysecret123456789')
      expect(c).toContain('page=2')
    })
    it('JSON 바디의 비밀번호·토큰 필드를 마스킹한다', () => {
      const py = generateCode(
        {
          method: 'POST',
          url: 'https://x.io/login',
          requestHeaders: { 'content-type': 'application/json' },
          requestPostData: '{"user":"me","password":"hunter2hunter2","nested":{"refresh_token":"rt-0123456789abcdef"}}'
        },
        { lang: 'python' }
      )
      expect(py).not.toContain('hunter2hunter2')
      expect(py).not.toContain('rt-0123456789abcdef')
      expect(py).toContain('me')
    })
    it('form 바디의 민감 필드를 마스킹한다', () => {
      const c = generateCode(
        {
          method: 'POST',
          url: 'https://x.io/login',
          requestHeaders: { 'content-type': 'application/x-www-form-urlencoded' },
          requestPostData: 'user=me&passwd=formsecret999'
        },
        { lang: 'curl' }
      )
      expect(c).not.toContain('formsecret999')
      expect(c).toContain('user=me')
    })
    it('민감하지 않은 쿠키/헤더는 보존한다', () => {
      const py = generateCode(jsonPost, { lang: 'python' })
      expect(py).toContain('dark') // theme cookie not masked
    })
  })

  describe('generateCode languages', () => {
    it('python: JSON 바디를 data=로 보내고 form으로 보내지 않는다', () => {
      const py = generateCode(jsonPost, { lang: 'python', maskSecrets: false })
      expect(py).toContain('import requests')
      expect(py).toContain('data = "{\\"item\\":\\"x\\",\\"qty\\":2}"')
      expect(py).toContain('resp = s.post(')
    })
    it('form 바디는 python dict로 파싱한다', () => {
      const py = generateCode(
        {
          method: 'POST',
          url: 'https://x.io/login',
          requestHeaders: { 'content-type': 'application/x-www-form-urlencoded' },
          requestPostData: 'user=me&pw=secret'
        },
        { lang: 'python', maskSecrets: false }
      )
      expect(py).toContain('"user": "me"')
      expect(py).toContain('"pw": "secret"')
    })
    it('curl 출력', () => {
      const c = generateCode(jsonPost, { lang: 'curl', maskSecrets: false })
      expect(c).toContain('curl -X POST')
      expect(c).toContain("-H 'Content-Type: application/json'")
      expect(c).toContain("-b 'SESSIONID=abcdef1234567890; theme=dark'")
      expect(c).toContain("--data-raw '{\"item\":\"x\",\"qty\":2}'")
    })
    it('curl: 캡처 값의 $()·백틱이 셸에서 실행되지 않게 작은따옴표로 감싼다', () => {
      const c = generateCode(
        {
          method: 'POST',
          url: 'https://x.io/a?q=$(id)',
          requestHeaders: { 'x-note': "it's `whoami`" },
          requestPostData: '{"u":"$(touch /tmp/pwned)"}'
        },
        { lang: 'curl', maskSecrets: false }
      )
      expect(c).toContain("'https://x.io/a?q=$(id)'")
      expect(c).toContain("-H 'x-note: it'\\''s `whoami`'")
      expect(c).toContain("--data-raw '{\"u\":\"$(touch /tmp/pwned)\"}'")
    })
    it('python: 문자열 안의 ": true"를 True로 바꾸지 않는다', () => {
      const py = generateCode(
        { method: 'GET', url: 'https://x.io/', requestHeaders: { 'x-note': 'a: true, b: null' } },
        { lang: 'python', maskSecrets: false }
      )
      expect(py).toContain('"x-note": "a: true, b: null"')
    })
    it('typescript 출력', () => {
      const ts = generateCode(jsonPost, { lang: 'typescript', maskSecrets: false })
      expect(ts).toContain('await fetch(')
      expect(ts).toContain('method: "POST"')
      expect(ts).toContain("credentials: 'include'")
      expect(ts).toContain('body:')
    })
  })
})
