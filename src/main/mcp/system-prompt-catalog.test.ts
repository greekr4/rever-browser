import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import { TOOL_BUCKETS, isAdvertised } from './tool-profile'

const PROMPT = readFileSync(
  path.join(path.dirname(fileURLToPath(import.meta.url)), '../../renderer/src/ai/system-prompt.md'),
  'utf8'
)

const NO_OP =
  "`bp_*` breakpoints are a no-op in this build — execution never pauses under Electron's `webContents.debugger` (even a literal `debugger;` won't halt). Instrumentation is not a fallback here, it is the only path — do not plan around a pause that will never come."

describe('system prompt catalog', () => {
  it('browse에 없는 도구 이름을 본문에 적지 않고 no-op 문장과 중단점 폴백 삭제를 유지한다', () => {
    expect(PROMPT).toContain(NO_OP)
    expect(PROMPT).not.toContain('breakpoint → runtime hook')
    expect(PROMPT).toContain('tool_profile_set({ profile: "reverse" })')
    const hits = Object.keys(TOOL_BUCKETS).filter(
      (name) => !isAdvertised(name, 'browse') && new RegExp(`\\b${name}\\b`).test(PROMPT)
    )
    expect(hits).toEqual([])
  })
})
