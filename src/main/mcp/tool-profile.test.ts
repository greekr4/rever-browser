import { readdirSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import { ALWAYS_RISKY_TOOLS } from '../../shared/risky-tools'
import {
  PROFILE_TOOL_BUDGET,
  TOOL_BUCKETS,
  TOOL_PROFILE_PROBE_REJECTED,
  TOOL_PROFILES,
  advertisedNames,
  assertProfileBudget,
  catalogFromHeader,
  isAdvertised,
  readToolProfile,
  toolProfileReadText,
  toolProfileSwitchedText,
  toolProfileTurnLine
} from './tool-profile'

const BROWSE_REVERSE = [
  'console_eval',
  'console_get_props',
  'cookie_list',
  'cookie_set',
  'cookie_delete',
  'list_macros',
  'get_macro',
  'create_macro',
  'update_macro',
  'remove_macro_steps',
  'run_macro',
  'delete_macro',
  'cli_arm',
  'cli_compile',
  'cli_list',
  'cli_describe',
  'cli_run',
  'cli_writes',
  'cli_export'
] as const

const HIDDEN = [
  'bp_add',
  'bp_list',
  'bp_remove',
  'bp_status',
  'bp_resume',
  'bp_step_over',
  'bp_step_into',
  'bp_step_out',
  'bp_eval_in_frame'
] as const

function sorted(names: Iterable<string>): string[] {
  return [...names].sort()
}

/** Source `registerTool` names, with storage.ts `kind` templates expanded. */
function sourceToolNames(): Set<string> {
  const dir = path.join(path.dirname(fileURLToPath(import.meta.url)), 'tools')
  const names = new Set<string>()
  const unmatched: string[] = []
  for (const file of readdirSync(dir)) {
    if (!file.endsWith('.ts') || file.endsWith('.test.ts')) continue
    const text = readFileSync(path.join(dir, file), 'utf8')
    for (const match of text.matchAll(/registerTool\(\s*([^,\n]+)/g)) {
      const arg = match[1].trim()
      const literal = arg.match(/^['"]([a-z0-9_]+)['"]$/)
      if (literal) {
        names.add(literal[1])
        continue
      }
      const template = arg.match(/^`\$\{kind\}_storage_(get|set|delete|clear)`$/)
      if (template) {
        names.add(`local_storage_${template[1]}`)
        names.add(`session_storage_${template[1]}`)
        continue
      }
      unmatched.push(`${file}: ${arg}`)
    }
  }
  expect(unmatched, 'registerTool 인자 중 해석하지 못한 것').toEqual([])
  return names
}

describe('tool-profile', () => {
  describe('레지스트리 키', () => {
    it('소스 registerTool 이름과 레지스트리 키가 같다', () => {
      const source = sourceToolNames()
      expect(sorted(Object.keys(TOOL_BUCKETS))).toEqual(sorted(source))
      expect(source.size).toBe(162)
      expect(source.has('tool_profile_set')).toBe(true)
    })
  })

  describe('광고 수', () => {
    it('browse 80, reverse 113, probe 37이고 예산 120을 넘지 않는다', () => {
      expect(PROFILE_TOOL_BUDGET).toBe(120)
      expect(advertisedNames('browse')).toHaveLength(80)
      expect(advertisedNames('reverse')).toHaveLength(113)
      expect(advertisedNames('probe')).toHaveLength(37)
      for (const profile of TOOL_PROFILES) {
        expect(() => assertProfileBudget(profile)).not.toThrow()
      }
    })

    it('코어 도구는 세 프로파일에 모두 있고, 레지스트리에 없는 이름은 빠진다', () => {
      expect(isAdvertised('browser_snapshot', 'browse')).toBe(true)
      expect(isAdvertised('browser_snapshot', 'reverse')).toBe(true)
      expect(isAdvertised('browser_snapshot', 'probe')).toBe(true)
      expect(isAdvertised('grep_scripts', 'browse')).toBe(false)
      expect(isAdvertised('grep_scripts', 'reverse')).toBe(true)
      expect(isAdvertised('not_a_tool', 'browse')).toBe(false)
    })
  })

  describe('browse와 reverse의 겹침', () => {
    it('코어를 뺀 겹침은 19개 이름으로 고정된다', () => {
      const overlap = Object.keys(TOOL_BUCKETS).filter(
        (name) =>
          TOOL_BUCKETS[name] !== 'core' &&
          isAdvertised(name, 'browse') &&
          isAdvertised(name, 'reverse')
      )
      expect(sorted(overlap)).toEqual(sorted(BROWSE_REVERSE))
    })
  })

  describe('위험 도구', () => {
    it('ALWAYS_RISKY_TOOLS 6개는 probe에만 있다', () => {
      expect(ALWAYS_RISKY_TOOLS).toHaveLength(6)
      for (const name of ALWAYS_RISKY_TOOLS) {
        expect(isAdvertised(name, 'probe'), name).toBe(true)
        expect(isAdvertised(name, 'browse'), name).toBe(false)
        expect(isAdvertised(name, 'reverse'), name).toBe(false)
      }
    })
  })

  describe('bp_*', () => {
    it('9개는 hidden이고 어떤 프로파일에도 광고되지 않는다', () => {
      const hidden = Object.keys(TOOL_BUCKETS).filter((name) => {
        const bucket = TOOL_BUCKETS[name]
        return bucket !== 'core' && bucket.includes('hidden')
      })
      expect(sorted(hidden)).toEqual(sorted(HIDDEN))
      for (const name of HIDDEN) {
        for (const profile of TOOL_PROFILES) {
          expect(isAdvertised(name, profile), `${name} ${profile}`).toBe(false)
        }
      }
    })
  })

  describe('합집합', () => {
    it('세 프로파일의 광고 합은 hidden을 제외한 전부다', () => {
      const advertised = new Set<string>()
      for (const profile of TOOL_PROFILES) {
        for (const name of advertisedNames(profile)) advertised.add(name)
      }
      const hidden = new Set<string>(HIDDEN)
      expect(advertised.size + hidden.size).toBe(Object.keys(TOOL_BUCKETS).length)
      for (const name of Object.keys(TOOL_BUCKETS)) {
        expect(advertised.has(name) || hidden.has(name), name).toBe(true)
        expect(advertised.has(name) && hidden.has(name), name).toBe(false)
      }
    })
  })

  describe('advertisedNames extras', () => {
    it('hidden과 모르는 이름은 빼고, 다른 프로파일에서 이미 호출된 이름은 붙인다', () => {
      const names = advertisedNames('browse', [
        'grep_scripts',
        'bp_add',
        'not_a_tool',
        'browser_snapshot',
        'grep_scripts'
      ])
      expect(names).toContain('grep_scripts')
      expect(names).not.toContain('bp_add')
      expect(names).not.toContain('not_a_tool')
      expect(names.filter((name) => name === 'browser_snapshot')).toHaveLength(1)
      expect(names.filter((name) => name === 'grep_scripts')).toHaveLength(1)
      expect(names).toHaveLength(81)
    })
  })

  describe('readToolProfile', () => {
    it('version이 1이고 profile이 세 값이면 그 프로파일을 반환한다', () => {
      expect(readToolProfile('{"version":1,"profile":"browse"}')).toBe('browse')
      expect(readToolProfile('{"version":1,"profile":"reverse","extra":true}')).toBe('reverse')
      expect(readToolProfile('{"version":1,"profile":"probe"}')).toBe('probe')
    })

    it('깨진 JSON, 다른 version, 알 수 없는 profile은 null이다', () => {
      expect(readToolProfile('')).toBeNull()
      expect(readToolProfile('{')).toBeNull()
      expect(readToolProfile('null')).toBeNull()
      expect(readToolProfile('[]')).toBeNull()
      expect(readToolProfile('{"version":2,"profile":"browse"}')).toBeNull()
      expect(readToolProfile('{"version":"1","profile":"browse"}')).toBeNull()
      expect(readToolProfile('{"version":1,"profile":"full"}')).toBeNull()
      expect(readToolProfile('{"version":1}')).toBeNull()
      expect(readToolProfile('{"profile":"browse"}')).toBeNull()
    })
  })

  describe('카탈로그와 반환 문자열', () => {
    it('헤더는 follow와 direct만 받고 나머지는 full이다', () => {
      expect(catalogFromHeader('follow')).toBe('follow')
      expect(catalogFromHeader('direct')).toBe('direct')
      expect(catalogFromHeader(undefined)).toBe('full')
      expect(catalogFromHeader('full')).toBe('full')
      expect(catalogFromHeader('Follow')).toBe('full')
      expect(catalogFromHeader('DIRECT')).toBe('full')
      expect(catalogFromHeader('')).toBe('full')
      expect(catalogFromHeader(['direct'])).toBe('direct')
      expect(catalogFromHeader(['Follow'])).toBe('full')
    })

    it('매 턴 한 줄은 프로파일 이름만 바뀐다', () => {
      expect(toolProfileTurnLine('browse')).toBe(
        '[tool-profile: browse] Tools outside this profile are not loaded. Call tool_profile_set to switch between browse and reverse. Probe is selected by the user.'
      )
      expect(toolProfileTurnLine('reverse')).toContain('[tool-profile: reverse]')
      expect(toolProfileTurnLine('probe')).toContain('[tool-profile: probe]')
    })

    it('읽기·전환·probe 거부 문자열이 고정이다', () => {
      expect(toolProfileReadText('browse', 77)).toBe(
        'profile=browse advertised=77. Switch with profile "browse" or "reverse". You cannot select probe; the user sets Tools → Probe. bp_* is not in any profile.'
      )
      expect(toolProfileSwitchedText('reverse', 109)).toBe(
        'profile=reverse advertised=109. If grep_scripts is not in your tool list yet, ask the user to send one more message.'
      )
      expect(TOOL_PROFILE_PROBE_REJECTED).toBe(
        'probe is user-selected. Ask the user to set Tools → Probe. Profile was not changed.'
      )
    })
  })
})
