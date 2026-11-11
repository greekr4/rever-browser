import { describe, expect, it } from 'vitest'

import { isInterestingStatus } from './path_probe'

// A hidden path is one whose response differs from the app's "absent" baseline.
// This is what lets an unlinked /healthcheck (400) or /healthcheck-dev (403 ACL)
// surface instead of being dropped by a 2xx-only filter.
describe('isInterestingStatus', () => {
  it('404-baseline app: any non-404 existing/guarded response is interesting', () => {
    expect(isInterestingStatus(200, 404)).toBe(true)
    expect(isInterestingStatus(400, 404)).toBe(true) // /healthcheck wants a param
    expect(isInterestingStatus(403, 404)).toBe(true) // /healthcheck-dev behind an ACL
    expect(isInterestingStatus(500, 404)).toBe(true)
    expect(isInterestingStatus(404, 404)).toBe(false) // same as baseline = absent
  })

  it('gated app (403 baseline): only responses differing from the gate surface', () => {
    expect(isInterestingStatus(403, 403)).toBe(false) // every path 403s — no signal
    expect(isInterestingStatus(200, 403)).toBe(true)
    expect(isInterestingStatus(400, 403)).toBe(true)
  })

  it('fetch error (status 0) is never interesting', () => {
    expect(isInterestingStatus(0, 404)).toBe(false)
    expect(isInterestingStatus(0, 0)).toBe(false)
  })

  it('no usable baseline: falls back to the 2xx/3xx heuristic (redirects excluded)', () => {
    expect(isInterestingStatus(200, 0)).toBe(true)
    expect(isInterestingStatus(403, 0)).toBe(false)
    expect(isInterestingStatus(301, 0)).toBe(false)
    expect(isInterestingStatus(302, 0)).toBe(false)
  })
})
