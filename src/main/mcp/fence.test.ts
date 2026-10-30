import { afterEach, describe, expect, it } from 'vitest'

import {
  acquireFence,
  releaseFence,
  isFenceActive,
  fenceReason,
  recordFenceBlock,
  getFenceBlocks
} from './fence'

afterEach(() => {
  // Drain any holders left by a test so the ref-count starts clean.
  while (isFenceActive()) releaseFence()
})

describe('fence', () => {
  describe('fenceReason', () => {
    it('비멱등 메서드는 mutation 으로 막는다', () => {
      for (const m of ['POST', 'PUT', 'PATCH', 'DELETE', 'post', 'delete']) {
        expect(fenceReason(m, 'https://x.test/api/items')).toBe('mutation')
      }
    })

    it('멱등 읽기는 통과시킨다', () => {
      for (const m of ['GET', 'HEAD', 'OPTIONS']) {
        expect(fenceReason(m, 'https://x.test/api/items')).toBeNull()
      }
    })

    it('인증 무효화 경로는 메서드와 무관하게 막는다', () => {
      expect(fenceReason('GET', 'https://x.test/logout')).toBe('auth-invalidation')
      expect(fenceReason('GET', 'https://x.test/auth/sign-out')).toBe('auth-invalidation')
      expect(fenceReason('GET', 'https://x.test/session/revoke')).toBe('auth-invalidation')
    })

    it('경로에 logout 유사 문자열이 포함돼도 토큰 경계가 아니면 막지 않는다', () => {
      expect(fenceReason('GET', 'https://x.test/products/logouter-brand')).toBeNull()
    })
  })

  describe('acquire/release', () => {
    it('홀더가 있을 때만 활성이고 참조 카운트로 동작한다', () => {
      expect(isFenceActive()).toBe(false)
      acquireFence()
      acquireFence()
      expect(isFenceActive()).toBe(true)
      releaseFence()
      expect(isFenceActive()).toBe(true) // 아직 한 홀더 남음
      releaseFence()
      expect(isFenceActive()).toBe(false)
    })

    it('첫 획득 시 지난 차단 원장을 비운다', () => {
      acquireFence()
      recordFenceBlock('POST', 'https://x.test/a', 'mutation')
      releaseFence()
      expect(getFenceBlocks()).toHaveLength(1)
      acquireFence() // fresh session clears
      expect(getFenceBlocks()).toHaveLength(0)
      releaseFence()
    })
  })
})
