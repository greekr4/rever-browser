// Locate the user's installed Claude Code CLI so claude-agent-acp drives it
// instead of the CLI bundled inside its @anthropic-ai/claude-agent-sdk
// dependency (0.2.83 → Claude Code 2.1.83). The bundled copy predates the
// Claude 5 models: the API rejects them with "Claude Code 2.1.83 does not
// support this model; version 2.1.280 or newer is required", and its
// `default` alias still resolves to Opus 4.6. claude-agent-acp honours
// CLAUDE_CODE_EXECUTABLE for the path it spawns, so we point it at the real CLI.

import { accessSync, constants } from 'node:fs'
import { delimiter, join } from 'node:path'
import { platform } from 'node:os'

import { extraDirs } from './acp-detect'

const isWindows = platform() === 'win32'

/**
 * Pure resolver: first `claude` candidate in `dirs` for which `isExecutable`
 * holds. Windows installs are `claude.exe` / `claude.cmd`.
 */
export function pickClaudeCli(
  dirs: string[],
  isExecutable: (p: string) => boolean,
  windows: boolean = isWindows
): string | null {
  const names = windows ? ['claude.exe', 'claude.cmd', 'claude'] : ['claude']
  for (const dir of dirs) {
    if (!dir) continue
    for (const name of names) {
      const candidate = join(dir, name)
      if (isExecutable(candidate)) return candidate
    }
  }
  return null
}

function isExecutableSync(p: string): boolean {
  try {
    accessSync(p, isWindows ? constants.F_OK : constants.X_OK)
    return true
  } catch {
    return false
  }
}

let cached: string | null | undefined

/** Resolve the user's `claude` binary once per process (PATH + common dirs). */
export function findClaudeCli(): string | null {
  if (cached !== undefined) return cached
  const dirs = [...(process.env.PATH ?? '').split(delimiter), ...extraDirs()]
  cached = pickClaudeCli(dirs, isExecutableSync)
  return cached
}
