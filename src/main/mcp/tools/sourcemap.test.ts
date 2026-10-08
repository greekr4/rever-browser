import { describe, expect, it, vi } from 'vitest'
import { TraceMap } from '@jridgewell/trace-mapping'

vi.mock('../../traffic-store', () => ({ getRequest: () => undefined }))

import { originalAt } from './sourcemap'

// 생성 코드 3번째 줄(0-based 2) 4번째 칸(0-based 4) → 원본 app.ts 11번째 줄(0-based 10) 2번째 칸
const tm = new TraceMap({
  version: 3,
  sources: ['app.ts'],
  names: [],
  mappings: ';;IAUE'
})

describe('sourcemap', () => {
  describe('originalAt', () => {
    it('0-based line/column을 받아 원본 위치를 찾는다 (trace-mapping은 1-based line)', () => {
      expect(originalAt(tm, 2, 4)).toMatchObject({ source: 'app.ts', line: 11, column: 2 })
    })
    it('매핑이 없는 줄은 source가 null이다', () => {
      expect(originalAt(tm, 1, 4).source).toBeNull()
    })
  })
})
