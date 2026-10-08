import { describe, expect, it } from 'vitest'

import {
  executedFunctions,
  isTraceableScript,
  lineIndex,
  offsetAt,
  positionAt,
  summarizeTrace,
  type ScriptCoverage
} from './exec-trace'

const SRC = ['function a(){', '  b()', '}', 'function b(){ return 1 }', 'a()'].join('\n')
// offsets: "function a(){\n" = 0..13, "  b()\n" = 14..19, "}\n" = 20..21, "function b(){ return 1 }\n" = 22..46, "a()" = 47..49

describe('exec-trace', () => {
  describe('positionAt / offsetAt', () => {
    it('UTF-16 offset을 1-based line:col로, 0-based line/col을 offset으로 바꾼다', () => {
      const idx = lineIndex(SRC)
      expect(positionAt(idx, 0)).toEqual({ line: 1, column: 1 })
      expect(positionAt(idx, 22)).toEqual({ line: 4, column: 1 })
      expect(positionAt(idx, 16)).toEqual({ line: 2, column: 3 })
      expect(offsetAt(idx, 1, 2)).toBe(16)
      expect(offsetAt(idx, 99, 0)).toBeNull()
    })
  })

  describe('isTraceableScript', () => {
    it('페이지 스크립트는 남긴다', () => {
      expect(isTraceableScript('https://x.io/app.js', false)).toBe(true)
      expect(isTraceableScript('http://localhost:8779/', false)).toBe(true)
    })
    it('Electron 내부·확장·devtools 스크립트는 항상 뺀다', () => {
      expect(isTraceableScript('node:electron/js2c/sandbox_bundle', true)).toBe(false)
      expect(isTraceableScript('chrome-extension://abc/x.js', true)).toBe(false)
      expect(isTraceableScript('devtools://devtools/x.js', true)).toBe(false)
    })
    it('URL 없는 스크립트(rever 자체 주입·eval)는 includeAnonymous일 때만 남긴다', () => {
      expect(isTraceableScript('', false)).toBe(false)
      expect(isTraceableScript('', true)).toBe(true)
    })
  })

  describe('executedFunctions', () => {
    const cov: ScriptCoverage = {
      scriptId: '9',
      url: 'https://x.io/app.js',
      functions: [
        { functionName: '', isBlockCoverage: true, ranges: [{ startOffset: 0, endOffset: 50, count: 1 }] },
        { functionName: 'a', isBlockCoverage: true, ranges: [{ startOffset: 0, endOffset: 21, count: 2 }] },
        { functionName: 'b', isBlockCoverage: false, ranges: [{ startOffset: 22, endOffset: 46, count: 0 }] }
      ]
    }
    it('구간 안에서 호출된(count>0) 함수만 남기고 스크립트 최상위는 뺀다', () => {
      expect(executedFunctions(cov)).toEqual([
        { name: 'a', start: 0, end: 21, calls: 2, blockCoverage: true }
      ])
    })
  })

  describe('summarizeTrace', () => {
    const coverage: ScriptCoverage[] = [
      {
        scriptId: '9',
        url: 'https://x.io/app.js',
        functions: [
          { functionName: 'a', isBlockCoverage: true, ranges: [{ startOffset: 0, endOffset: 21, count: 1 }] },
          { functionName: 'b', isBlockCoverage: true, ranges: [{ startOffset: 22, endOffset: 46, count: 3 }] }
        ]
      },
      {
        scriptId: '10',
        url: 'https://x.io/idle.js',
        functions: [{ functionName: 'z', isBlockCoverage: true, ranges: [{ startOffset: 0, endOffset: 5, count: 0 }] }]
      }
    ]
    const scripts = new Map([['9', { url: 'https://x.io/app.js', source: SRC, requestId: 'req-9' }]])
    const requests = [
      {
        requestId: 'r1',
        method: 'POST',
        url: 'https://x.io/api/sign',
        status: 200,
        // call site inside b() → line 4 col 15 (0-based 3, 14)
        initiatorStack: [{ functionName: 'b', url: 'https://x.io/app.js', lineNumber: 3, columnNumber: 14 }]
      }
    ]

    it('실행된 함수를 스크립트별 위치와 호출 횟수, resolve_source 인자로 정리한다', () => {
      const s = summarizeTrace({ coverage, scripts, requests, limit: 50 })
      expect(s.scripts).toHaveLength(1)
      expect(s.scripts[0].url).toBe('https://x.io/app.js')
      expect(s.scripts[0].functions.map((f) => [f.name, f.location, f.calls])).toEqual([
        ['a', 'https://x.io/app.js:1:1', 1],
        ['b', 'https://x.io/app.js:4:1', 3]
      ])
      expect(s.scripts[0].functions[1].resolveSource).toEqual({ requestId: 'req-9', line: 3, column: 0 })
    })

    it('요청의 initiator 스택이 지나간 함수를 onRequestStack으로 표시한다', () => {
      const s = summarizeTrace({ coverage, scripts, requests, limit: 50 })
      const b = s.scripts[0].functions.find((f) => f.name === 'b')
      expect(b?.onRequestStack).toEqual(['r1'])
      expect(s.scripts[0].functions.find((f) => f.name === 'a')?.onRequestStack).toEqual([])
      expect(s.requests[0]).toMatchObject({ requestId: 'r1', method: 'POST', status: 200 })
    })

    it('limit을 넘는 함수는 자르고 잘린 개수를 알려준다', () => {
      const s = summarizeTrace({ coverage, scripts, requests, limit: 1 })
      expect(s.totalFunctions).toBe(2)
      expect(s.omittedFunctions).toBe(1)
    })

    it('원문이 없는 스크립트는 offset 위치로 표시한다', () => {
      const s = summarizeTrace({ coverage, scripts: new Map(), requests: [], limit: 50 })
      expect(s.scripts[0].functions[0].location).toBe('https://x.io/app.js@0')
    })
  })
})
