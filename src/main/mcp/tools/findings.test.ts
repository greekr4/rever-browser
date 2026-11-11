import { describe, expect, it } from 'vitest'

import { reportBucket, maskPii } from './findings'

// The report is a deliverable; applicant PII must never reach it. One scan
// pasted real emails/phones into the body (regression), so masking is enforced
// deterministically rather than left to the worker.
describe('maskPii', () => {
  it('이메일은 앞 2글자 + 도메인만 남긴다 (실제 유출값)', () => {
    expect(maskPii('email rnjstnfls00@naver.com here')).toBe('email rn****@naver.com here')
    expect(maskPii('greekr3@nate.com')).toBe('gr****@nate.com')
  })

  it('한국 전화번호를 가린다 (하이픈/무하이픈/지역번호)', () => {
    expect(maskPii('phone 010-1234-5678')).toBe('phone 010-****-****')
    expect(maskPii('01012345678')).toBe('010-****-****')
    expect(maskPii('02-123-4567')).toBe('02-****-****')
  })

  it('이미 가려진 값은 다시 건드리지 않는다 (멱등)', () => {
    const once = maskPii('uo****@naver.com 010-****-****')
    expect(maskPii(once)).toBe(once)
  })

  it('이메일이 아닌 평문·UUID·epoch 는 그대로 둔다', () => {
    expect(maskPii('GET /v1/ats/jobs/d5f67a7e-1 at 1794360739')).toBe('GET /v1/ats/jobs/d5f67a7e-1 at 1794360739')
  })
})

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
