import { describe, expect, it } from 'vitest'

import { formatEvalResult } from './console-format'

describe('formatEvalResult', () => {
  it('문자열은 따옴표로 감싼다', () => {
    expect(formatEvalResult({ result: { type: 'string', value: 'zz' } })).toEqual({
      text: '"zz"',
      isError: false
    })
  })

  it('undefined와 null을 그대로 보여준다', () => {
    expect(formatEvalResult({ result: { type: 'undefined' } }).text).toBe('undefined')
    expect(formatEvalResult({ result: { type: 'object', subtype: 'null', value: null } }).text).toBe('null')
  })

  it('숫자와 NaN 같은 직렬화 불가 값을 보여준다', () => {
    expect(formatEvalResult({ result: { type: 'number', value: 3, description: '3' } }).text).toBe('3')
    expect(formatEvalResult({ result: { type: 'number', unserializableValue: 'NaN', description: 'NaN' } }).text).toBe('NaN')
  })

  it('객체 미리보기를 {키: 값} 형태로 만든다', () => {
    const r = formatEvalResult({
      result: {
        type: 'object',
        description: 'Object',
        preview: {
          type: 'object',
          overflow: true,
          properties: [
            { name: 'a', type: 'number', value: '1' },
            { name: 'b', type: 'string', value: 'x' }
          ]
        }
      }
    })
    expect(r.text).toBe('{a: 1, b: "x", …}')
  })

  it('배열 미리보기를 [값] 형태로 만든다', () => {
    const r = formatEvalResult({
      result: {
        type: 'object',
        subtype: 'array',
        description: 'Array(2)',
        preview: {
          type: 'object',
          subtype: 'array',
          overflow: false,
          properties: [
            { name: '0', type: 'number', value: '1' },
            { name: '1', type: 'number', value: '2' }
          ]
        }
      }
    })
    expect(r.text).toBe('[1, 2]')
  })

  it('예외는 에러로 표시하고 스택 설명을 쓴다', () => {
    const r = formatEvalResult({
      result: { type: 'object', subtype: 'error' },
      exceptionDetails: {
        text: 'Uncaught',
        exception: { type: 'object', subtype: 'error', description: 'ReferenceError: x is not defined' }
      }
    })
    expect(r).toEqual({ text: 'Uncaught ReferenceError: x is not defined', isError: true })
  })
})
