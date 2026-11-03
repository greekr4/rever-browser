import { describe, expect, it } from 'vitest'

import {
  buildNosqliCases,
  nosqliEvaluated,
  NOSQL_ERROR_RE,
  injectKv,
  injectJson,
  type NosqliReply,
  type NosqliContext
} from './nosqli_probe'

const reply = (context: NosqliContext, nonce: string, body: string, status = 200): NosqliReply => ({
  context,
  nonce,
  body,
  status
})

describe('buildNosqliCases', () => {
  it('연산자 true/false 쌍 + 에러 브레이커를 만든다', () => {
    const cases = buildNosqliCases('zXYZz')
    expect(cases.map((c) => c.context)).toEqual(['op-true', 'op-false', 'error'])
    expect(cases[0]).toMatchObject({ op: 'ne', value: 'zXYZz' })
    expect(cases[1]).toMatchObject({ op: 'eq', value: 'zXYZz' })
    expect(cases[2].value).toContain("'\"")
  })
})

describe('injectKv', () => {
  it('폼/쿼리의 대상 파라미터를 연산자 절로 바꾼다', () => {
    expect(injectKv('username=rtuser&password=x', 'username', 'username[$ne]=zz')).toBe(
      'username[$ne]=zz&password=x'
    )
  })

  it('기존 연산자 표기도 교체한다', () => {
    expect(injectKv('username[$eq]=a&password=x', 'username', 'username[$ne]=zz')).toBe(
      'username[$ne]=zz&password=x'
    )
  })

  it('없으면 뒤에 덧붙인다', () => {
    expect(injectKv('password=x', 'username', 'username[$ne]=zz')).toBe('password=x&username[$ne]=zz')
  })

  it('접두사가 같은 다른 파라미터는 건드리지 않는다', () => {
    expect(injectKv('user=a&username=b', 'user', 'user[$ne]=zz')).toBe('user[$ne]=zz&username=b')
  })
})

describe('injectJson', () => {
  it('대상 키를 연산자 객체로 세팅한다', () => {
    expect(JSON.parse(injectJson('{"username":"a","password":"b"}', 'username', { $ne: 'zz' }))).toEqual({
      username: { $ne: 'zz' },
      password: 'b'
    })
  })

  it('스칼라 브레이커도 세팅한다', () => {
    expect(JSON.parse(injectJson('{"username":"a"}', 'username', "zz'\"//"))).toEqual({
      username: "zz'\"//"
    })
  })
})

describe('NOSQL_ERROR_RE', () => {
  it('Mongo/Neo4j 드라이버 에러에만 반응한다', () => {
    expect(NOSQL_ERROR_RE.test('Neo.ClientError.Statement.SyntaxError')).toBe(true)
    expect(NOSQL_ERROR_RE.test('MongoServerError: unknown operator: $where')).toBe(true)
    expect(NOSQL_ERROR_RE.test('Invalid Cypher query near RETURN')).toBe(true)
    expect(NOSQL_ERROR_RE.test('CastError: Cast to ObjectId failed')).toBe(true)
    expect(NOSQL_ERROR_RE.test('Invalid user or password')).toBe(false)
    expect(NOSQL_ERROR_RE.test('You have an error in your SQL syntax')).toBe(false)
  })
})

describe('nosqliEvaluated', () => {
  it('에러 응답에 드라이버 시그니처가 있으면 error 로 확정한다', () => {
    const out = nosqliEvaluated([
      reply('op-true', 'zz', 'same'),
      reply('op-false', 'zz', 'same'),
      reply('error', "zz'\"", '500 Neo.ClientError.Statement.SyntaxError', 500)
    ])
    expect(out).toEqual({ nosqli: true, contexts: ['error'] })
  })

  it('연산자 true/false 응답이 차등이면 operator 로 확정한다', () => {
    const out = nosqliEvaluated([
      reply('op-true', 'zz', 'Welcome admin dashboard', 302),
      reply('op-false', 'zz', 'Invalid user or password', 200),
      reply('error', "zz'\"", 'ok')
    ])
    expect(out).toEqual({ nosqli: true, contexts: ['operator'] })
  })

  it('nonce 반사만 다르면 차등이 아니다 — 오탐 방지', () => {
    const out = nosqliEvaluated([
      reply('op-true', 'zz', 'you searched for zz'),
      reply('op-false', 'zz', 'you searched for zz'),
      reply('error', "zz'\"", 'ok')
    ])
    expect(out.nosqli).toBe(false)
  })

  it('bcrypt 로그인처럼 세 응답이 동일하면 음성', () => {
    const out = nosqliEvaluated([
      reply('op-true', 'zz', 'Invalid user or password', 200),
      reply('op-false', 'zz', 'Invalid user or password', 200),
      reply('error', "zz'\"", 'Invalid user or password', 200)
    ])
    expect(out).toEqual({ nosqli: false, contexts: [] })
  })
})
