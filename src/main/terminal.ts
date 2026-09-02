import { spawn as ptySpawn, type IPty } from 'node-pty'
import { writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

import { app } from 'electron'

import { startMcpServer } from './mcp/server'

// A local CLI agent running in a real PTY (Orca-style "terminal mode"), as an
// alternative to the ACP transport. The rever MCP server is wired into the CLI
// so the terminal agent gets the same browser / traffic tools the ACP agent has.
//
// macOS/Linux for now (uses a POSIX login shell to resolve the agent binary).

const terminals = new Map<string, IPty>()
let seq = 0

// Write an MCP config the Claude Code CLI can consume (`claude --mcp-config`).
function writeMcpConfig(url: string, authHeader: string): string {
  const file = join(app.getPath('userData'), 'rever-cli-mcp.json')
  const cfg = { mcpServers: { rever: { type: 'http', url, headers: { Authorization: authHeader } } } }
  writeFileSync(file, JSON.stringify(cfg, null, 2), { mode: 0o600 })
  return file
}

function shellQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`
}

export interface SpawnTerminalOptions {
  cols: number
  rows: number
  // Agent to launch. 'claude' runs the Claude Code CLI wired to rever's MCP;
  // 'shell' just opens a plain login shell.
  agent: 'claude' | 'shell'
}

export async function spawnTerminal(
  opts: SpawnTerminalOptions,
  onData: (id: string, data: string) => void,
  onExit: (id: string, code: number) => void
): Promise<string> {
  const shell = process.env.SHELL || '/bin/zsh'
  const args: string[] = ['-il'] // interactive login: sources rc files so the agent binary is on PATH

  if (opts.agent === 'claude') {
    const mcp = await startMcpServer()
    const cfg = writeMcpConfig(mcp.url, mcp.authHeader)
    // -c runs the command in the login shell (PATH resolved), exec so the PTY
    // dies with the CLI. `--strict-mcp-config` keeps only our server so the
    // browser tools are always present regardless of the user's global config.
    args.push('-c', `exec claude --mcp-config ${shellQuote(cfg)} --strict-mcp-config`)
  }

  const pty = ptySpawn(shell, args, {
    name: 'xterm-color',
    cols: opts.cols || 80,
    rows: opts.rows || 24,
    cwd: homedir(),
    env: { ...process.env, TERM: 'xterm-256color' } as Record<string, string>
  })

  const id = `term${++seq}`
  terminals.set(id, pty)
  pty.onData((d) => onData(id, d))
  pty.onExit(({ exitCode }) => {
    terminals.delete(id)
    onExit(id, exitCode)
  })
  return id
}

export function writeTerminal(id: string, data: string): void {
  terminals.get(id)?.write(data)
}

export function resizeTerminal(id: string, cols: number, rows: number): void {
  try {
    terminals.get(id)?.resize(Math.max(1, cols), Math.max(1, rows))
  } catch {
    // PTY already exited — ignore.
  }
}

export function killTerminal(id: string): void {
  const pty = terminals.get(id)
  if (!pty) return
  terminals.delete(id)
  try {
    pty.kill()
  } catch {
    // Already dead.
  }
}

export function killAllTerminals(): void {
  for (const id of [...terminals.keys()]) killTerminal(id)
}
