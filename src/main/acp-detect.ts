import { access, constants } from 'node:fs/promises'
import { delimiter, join } from 'node:path'
import { homedir, platform } from 'node:os'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execFileP = promisify(execFile)

export interface AgentProbe {
  /** Primary binary to look for. */
  command: string
  /** Drop-in forks tried if `command` isn't on PATH. */
  fallbackBins?: string[]
}

export interface AgentProbeResult {
  /** Original command requested. */
  command: string
  /** Absolute path that was found, or null. */
  resolvedPath: string | null
  /** Which bin actually matched (command or one of the fallbacks). */
  matchedBin: string | null
}

const isWindows = platform() === 'win32'

/** Windows PATHEXT values, lowercased, with '' for already-suffixed names. */
function getWindowsExts(): string[] {
  const raw = process.env.PATHEXT ?? '.COM;.EXE;.BAT;.CMD'
  return ['', ...raw.split(';').map((e) => e.toLowerCase()).filter(Boolean)]
}

function getPathDirs(): string[] {
  const fromEnv = (process.env.PATH ?? '').split(delimiter).filter(Boolean)
  return [...fromEnv, ...extraDirs()]
}

/** Augment PATH with locations commonly missing from Electron's env. */
export function extraDirs(): string[] {
  const home = homedir()
  if (isWindows) {
    const appData = process.env.APPDATA
    const programFiles = process.env.ProgramW6432 || process.env.ProgramFiles
    const programFilesX86 = process.env['ProgramFiles(x86)']
    const localAppData = process.env.LOCALAPPDATA
    return [
      appData ? join(appData, 'npm') : '',
      join(home, 'AppData', 'Roaming', 'npm'),
      join(home, 'scoop', 'shims'),
      join(home, '.bun', 'bin'),
      // npm's Windows CLI shims (claude-code-acp.cmd) run `node <adapter>` and
      // fall back to `node` on PATH when there's no node.exe beside the shim.
      // A GUI/packaged launch can hand Electron a PATH without Node's install
      // dir, so the shim exits with "'node' is not recognized" the instant it
      // starts — the agent's stdout closes before the handshake and the SDK
      // reports the opaque "ACP connection closed". Add the common Node install
      // dirs so the shim can always resolve `node`.
      programFiles ? join(programFiles, 'nodejs') : '',
      programFilesX86 ? join(programFilesX86, 'nodejs') : '',
      localAppData ? join(localAppData, 'Volta', 'bin') : '',
      // nvm-windows points this at its active version's dir (usually the
      // C:\Program Files\nodejs symlink, but honour an explicit override).
      process.env.NVM_SYMLINK || ''
    ].filter(Boolean)
  }
  return [
    '/opt/homebrew/bin',
    '/usr/local/bin',
    '/usr/bin',
    join(home, '.local', 'bin'),
    join(home, '.bun', 'bin'),
    join(home, '.cargo', 'bin'),
    join(home, '.npm-global', 'bin')
  ]
}

async function isExecutable(p: string): Promise<boolean> {
  try {
    await access(p, isWindows ? constants.F_OK : constants.X_OK)
    return true
  } catch {
    return false
  }
}

/** Resolve a single bin name to an absolute path, or null if not found. */
async function which(bin: string, dirs: string[], exts: string[]): Promise<string | null> {
  for (const dir of dirs) {
    for (const ext of exts) {
      const candidate = join(dir, bin + ext)
      if (await isExecutable(candidate)) return candidate
    }
  }
  return null
}

/**
 * Resolve a list of agent probes against PATH. For each entry, tries
 * `command` first, then `fallbackBins` in order. Pure read — never spawns
 * the binary, just stat-checks files.
 */
export async function detectAgents(probes: AgentProbe[]): Promise<AgentProbeResult[]> {
  const dirs = getPathDirs()
  const exts = isWindows ? getWindowsExts() : ['']

  // Also try `npm prefix -g` once and prepend its bin/ — picks up custom
  // npm prefixes (nvm, volta) that aren't on PATH inside Electron.
  try {
    const { stdout } = await execFileP(isWindows ? 'npm.cmd' : 'npm', ['prefix', '-g'], {
      timeout: 2_000
    })
    const prefix = stdout.trim()
    if (prefix) {
      dirs.unshift(isWindows ? prefix : join(prefix, 'bin'))
    }
  } catch {
    // npm not on PATH — skip silently
  }

  return Promise.all(
    probes.map(async (probe) => {
      const candidates = [probe.command, ...(probe.fallbackBins ?? [])]
      for (const bin of candidates) {
        const resolved = await which(bin, dirs, exts)
        if (resolved) {
          return { command: probe.command, resolvedPath: resolved, matchedBin: bin }
        }
      }
      return { command: probe.command, resolvedPath: null, matchedBin: null }
    })
  )
}
