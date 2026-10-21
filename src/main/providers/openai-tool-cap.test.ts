import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, it, expect } from 'vitest'

import { advertisedNames, isAdvertised } from '../mcp/tool-profile'
import { capToolsForOpenAI, OPENAI_MAX_TOOLS, OPENAI_DROP_ORDER } from './openai-tool-cap'

const tool = (name: string) => ({ type: 'function' as const, function: { name } })

describe('capToolsForOpenAI', () => {
  it('leaves a list at or under the cap untouched', () => {
    const tools = Array.from({ length: OPENAI_MAX_TOOLS }, (_, i) => tool(`t${i}`))
    const r = capToolsForOpenAI(tools)
    expect(r.tools).toHaveLength(OPENAI_MAX_TOOLS)
    expect(r.dropped).toEqual([])
  })

  it('drops denylist tools first, in order, only as many as needed (140 -> 128)', () => {
    const names = [
      ...OPENAI_DROP_ORDER,
      ...Array.from({ length: 129 }, (_, i) => `keep${i}`)
    ]
    expect(names).toHaveLength(140)
    const r = capToolsForOpenAI(names.map(tool))
    expect(r.tools).toHaveLength(128)
    expect(r.dropped).toEqual([...OPENAI_DROP_ORDER, 'keep128'])
    expect(r.tools.every((t) => !r.dropped.includes(t.function.name))).toBe(true)
  })

  it('ignores denylist names that are not present', () => {
    // 129 tools, only one denylist name present -> that one drops, list fits.
    const names = ['bp_add', ...Array.from({ length: 128 }, (_, i) => `keep${i}`)]
    const r = capToolsForOpenAI(names.map(tool))
    expect(r.tools).toHaveLength(128)
    expect(r.dropped).toEqual(['bp_add'])
  })

  it('falls back to dropping from the tail when the denylist is exhausted', () => {
    const names = Array.from({ length: 130 }, (_, i) => `x${i}`)
    const r = capToolsForOpenAI(names.map(tool))
    expect(r.tools).toHaveLength(128)
    expect(r.dropped).toEqual(['x128', 'x129'])
  })

  it('꼬리의 core 도구는 남기고 그 앞의 비core를 자른다', () => {
    const names = [...Array.from({ length: 128 }, (_, i) => `x${i}`), 'browser_snapshot']
    const r = capToolsForOpenAI(names.map(tool))
    expect(r.tools).toHaveLength(128)
    expect(r.dropped).toEqual(['x127'])
    expect(r.tools.map((t) => t.function.name)).toContain('browser_snapshot')
  })

  it('reverse에 browse 전용 34개를 더한 147개에서 등록 순 꼬리 19개만 떨어진다', () => {
    const order = registrationOrder()
    expect(order).toHaveLength(162)
    const browseOnly = advertisedNames('browse').filter((name) => !isAdvertised(name, 'reverse'))
    expect(browseOnly).toHaveLength(34)
    const allowed = new Set([...advertisedNames('reverse'), ...browseOnly])
    expect(allowed.size).toBe(147)
    const filtered = order.filter((name) => allowed.has(name))
    expect(filtered).toHaveLength(147)
    expect(filtered.at(-1)).toBe('tool_profile_set')
    const r = capToolsForOpenAI(filtered.map(tool))
    expect(r.tools).toHaveLength(128)
    expect(r.dropped).toEqual([
      'dialog_inject_override',
      'dialog_clear_history',
      'crypto_trace_stop',
      'fetch_body',
      'detect_antibot_vm',
      'graphql_introspect',
      'protobuf_decode',
      'list_wasm',
      'wasm_decompile',
      'wasm_info',
      'grep_wasm',
      'wasm_xref',
      'cli_arm',
      'cli_compile',
      'cli_list',
      'cli_describe',
      'cli_run',
      'cli_writes',
      'cli_export'
    ])
    const kept = r.tools.map((t) => t.function.name)
    expect(kept).toContain('browser_snapshot')
    expect(kept).toContain('tool_profile_set')
    expect(kept).toContain('crypto_trace_start')
  })
})

function registrationOrder(): string[] {
  const mcpDir = path.join(path.dirname(fileURLToPath(import.meta.url)), '../mcp')
  const server = readFileSync(path.join(mcpDir, 'server.ts'), 'utf8')
  const imports = new Map<string, string>()
  for (const match of server.matchAll(/import \{ (register\w+) \} from '\.\/tools\/([^']+)'/g)) {
    imports.set(match[1], match[2])
  }
  const start = server.indexOf('export function buildMcpServer')
  const end = server.indexOf('\ninterface RunningServer')
  const body = server.slice(start, end)
  const names: string[] = []
  for (const match of body.matchAll(/\b(register\w+)\(mcp\b/g)) {
    const file = imports.get(match[1])
    if (!file) throw new Error(`no import for ${match[1]}`)
    const text = readFileSync(path.join(mcpDir, 'tools', `${file}.ts`), 'utf8')
    names.push(...toolNamesInSource(text, file))
  }
  return names
}

function toolNamesInSource(text: string, file: string): string[] {
  const names: string[] = []
  const loopAt = text.search(/for\s*\(\s*const\s+kind\s+of\s+\['local',\s*'session'\]/)
  const head = loopAt === -1 ? text : text.slice(0, loopAt)
  const tail = loopAt === -1 ? '' : text.slice(loopAt)
  for (const match of head.matchAll(/registerTool\(\s*([^,\n]+)/g)) {
    const arg = match[1].trim()
    const literal = arg.match(/^['"]([a-z0-9_]+)['"]$/)
    if (!literal) throw new Error(`${file}: unrecognized registerTool arg: ${arg}`)
    names.push(literal[1])
  }
  if (!tail) return names
  const suffixes: string[] = []
  for (const match of tail.matchAll(/registerTool\(\s*([^,\n]+)/g)) {
    const arg = match[1].trim()
    const template = arg.match(/^`\$\{kind\}_storage_(get|set|delete|clear)`$/)
    if (!template) throw new Error(`${file}: unrecognized registerTool arg in kind loop: ${arg}`)
    suffixes.push(template[1])
  }
  for (const kind of ['local', 'session']) {
    for (const suffix of suffixes) names.push(`${kind}_storage_${suffix}`)
  }
  return names
}
