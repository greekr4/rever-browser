import { describe, expect, it } from 'vitest'

import { reportBucket } from './findings'

// reportBucket decides whether a finding is a confirmed fact, an unproven
// suspicion to verify, or attack-surface inventory — the split that keeps a
// read-only scan's report from reading as 12 equal "findings".
describe('reportBucket', () => {
  const f = (over: { category?: string; severity?: string; status?: string }) =>
    ({ category: 'vuln', severity: 'medium', status: 'candidate', ...over }) as Parameters<typeof reportBucket>[0]

  it('엔드포인트 인벤토리는 심각도와 무관하게 정찰(recon)', () => {
    expect(reportBucket(f({ category: 'endpoint', severity: 'medium' }))).toBe('recon')
    expect(reportBucket(f({ category: 'endpoint', status: 'validated' }))).toBe('recon')
  })

  it('info 심각도와 accepted_risk 는 정찰로 내린다 (헤드라인 오염 방지)', () => {
    expect(reportBucket(f({ category: 'auth', severity: 'info' }))).toBe('recon')
    expect(reportBucket(f({ category: 'vuln', status: 'accepted_risk' }))).toBe('recon')
  })

  it('validated 보안 이슈만 confirmed', () => {
    expect(reportBucket(f({ category: 'vuln', status: 'validated' }))).toBe('confirmed')
    expect(reportBucket(f({ category: 'secret', status: 'validated' }))).toBe('confirmed')
  })

  it('실증 안 된 보안 이슈는 candidate — 실제 두 건(cookie-scope·headers)이 여기로', () => {
    expect(reportBucket(f({ category: 'auth', severity: 'medium', status: 'candidate' }))).toBe('candidate')
    expect(reportBucket(f({ category: 'other', severity: 'medium', status: 'candidate' }))).toBe('candidate')
  })
})
