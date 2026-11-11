import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterAll, describe, expect, it } from 'vitest'

import { claudeCliArgs } from './providers/claude-cli-protocol'
import { quoteCmdArg, shellSafeSpawn } from './win-shell'

describe('shellSafeSpawn', () => {
  it('leaves POSIX launches alone', () => {
    expect(shellSafeSpawn('claude', ['--setting-sources', ''], false)).toEqual({
      command: 'claude',
      args: ['--setting-sources', ''],
      shell: false
    })
  })

  it('keeps empty arguments as a quoted pair on Windows', () => {
    expect(quoteCmdArg('')).toBe('^"^"')
    expect(shellSafeSpawn('C:\\Program Files\\claude.exe', ['--setting-sources', ''], true)).toEqual({
      command: '"C:\\Program Files\\claude.exe" ^"--setting-sources^" ^"^"',
      args: [],
      shell: true
    })
  })

  it('escapes embedded quotes and trailing backslashes per MSVCRT rules', () => {
    expect(quoteCmdArg('a"b')).toBe('^"a\\^"b^"')
    expect(quoteCmdArg('dir\\')).toBe('^"dir\\\\^"')
  })
})

// Real cmd.exe round trip: what the child sees in argv must equal what we passed.
describe.runIf(process.platform === 'win32')('cmd.exe round trip', () => {
  const dir = mkdtempSync(join(tmpdir(), 'rever win shell '))
  afterAll(() => rmSync(dir, { recursive: true, force: true }))

  const script = join(dir, 'echo-argv.js')
  writeFileSync(script, 'process.stdout.write(JSON.stringify(process.argv.slice(2)))')
  // Same shape as the shim npm writes for a global CLI.
  const shim = join(dir, 'echo argv.cmd')
  writeFileSync(shim, `@"${process.execPath}" "${script}" %*\r\n`)

  const args = [
    ...claudeCliArgs({ mcpUrl: 'http://127.0.0.1:4321/mcp', authHeader: 'Bearer abc123' }),
    '',
    'x & y | z',
    '%PATH%',
    'say "hi" (now)!',
    'C:\\dir with space\\'
  ]

  const run = (command: string, argv: string[]): string[] => {
    const launch = shellSafeSpawn(command, argv, true)
    const r = spawnSync(launch.command, launch.args, { shell: launch.shell, encoding: 'utf8' })
    expect(r.stderr).toBe('')
    return JSON.parse(r.stdout)
  }

  it('passes every argument intact to an .exe', () => {
    expect(run(process.execPath, [script, ...args])).toEqual(args)
  })

  it('passes every argument intact through an npm-style .cmd shim', () => {
    // %VAR% inside a batch %* line is re-expanded by cmd; that is a shim limit, not ours.
    const viaShim = args.filter((a) => !a.includes('%'))
    expect(run(shim, viaShim)).toEqual(viaShim)
  })
})
