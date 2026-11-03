import { describe, expect, it } from 'vitest'

import { buildSqliCases, sqliEvaluated, SQL_ERROR_RE, type SqliReply } from './sqli_probe'

const reply = (context: string, payload: string, body: string, status = 200): SqliReply => ({
  context,
  payload,
  body,
  status
})

describe('buildSqliCases', () => {
  it('quote 1개 + boolean 산술 true/false 쌍을 만든다', () => {
    const cases = buildSqliCases(101, 103, 'zXYZz')
    expect(cases.map((c) => c.context)).toEqual(['error', 'bool-true', 'bool-false'])
    expect(cases[0].payload).toBe("zXYZz'")
    expect(cases[1].payload).toBe(`zXYZz' OR 101*103=${101 * 103}-- -`)
    expect(cases[2].payload).toBe(`zXYZz' OR 101*103=${101 * 103 + 1}-- -`)
    // true/false 는 비교값만 다르다 (한쪽만 참)
    expect(cases[1].payload.length).toBe(cases[2].payload.length)
  })
})

describe('SQL_ERROR_RE', () => {
  it('DB 드라이버 에러 시그니처에만 반응한다', () => {
    expect(SQL_ERROR_RE.test('{"error":{"type":"ProgrammingError","message":"You have an error in your SQL syntax"}}')).toBe(true)
    expect(SQL_ERROR_RE.test('mysql.connector.errors.ProgrammingError')).toBe(true)
    expect(SQL_ERROR_RE.test('(sqlite3.OperationalError) near "z": syntax error')).toBe(true)
    expect(SQL_ERROR_RE.test('Invalid user or password')).toBe(false)
    expect(SQL_ERROR_RE.test('Welcome to the secure terminal')).toBe(false)
  })
})

describe('sqliEvaluated', () => {
  const [, t, f] = buildSqliCases(101, 103, 'zXYZz')

  it('에러 응답에 SQL 시그니처가 있으면 error 컨텍스트로 확정한다', () => {
    const out = sqliEvaluated([reply('error', "zXYZz'", '500: ProgrammingError sql syntax', 500), reply('bool-true', t.payload, 'x'), reply('bool-false', f.payload, 'x')])
    expect(out).toEqual({ sqli: true, contexts: ['error'] })
  })

  it('true/false 응답이 차등이면 boolean 컨텍스트로 확정한다', () => {
    const out = sqliEvaluated([
      reply('error', "zXYZz'", 'Not Found'),
      reply('bool-true', t.payload, '3 results for admin', 200),
      reply('bool-false', f.payload, '0 results', 200)
    ])
    expect(out).toEqual({ sqli: true, contexts: ['boolean'] })
  })

  it('페이로드가 리터럴로 반사되기만 하면(에코 차이 1글자) 차등이 아니다 — 오탐 방지', () => {
    const echo = (p: string) => `search echoed: ${p}` // true/false 페이로드의 6 vs 7 한 글자만 다름
    const out = sqliEvaluated([
      reply('error', "zXYZz'", 'no results'),
      reply('bool-true', t.payload, echo(t.payload)),
      reply('bool-false', f.payload, echo(f.payload))
    ])
    expect(out.sqli).toBe(false)
  })

  it('HTML 이스케이프된 반사도 에코 변형 제거로 중립화된다', () => {
    const echo = (p: string) => `value: ${p.replaceAll("'", '&#39;')}`
    const out = sqliEvaluated([
      reply('error', "zXYZz'", 'ok'),
      reply('bool-true', t.payload, echo(t.payload)),
      reply('bool-false', f.payload, echo(f.payload))
    ])
    expect(out.sqli).toBe(false)
  })

  it('세 응답이 동일하면 음성', () => {
    const out = sqliEvaluated([
      reply('error', "zXYZz'", 'Invalid user or password', 401),
      reply('bool-true', t.payload, 'Invalid user or password', 401),
      reply('bool-false', f.payload, 'Invalid user or password', 401)
    ])
    expect(out).toEqual({ sqli: false, contexts: [] })
  })
})
