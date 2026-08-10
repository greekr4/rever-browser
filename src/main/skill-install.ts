import { existsSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

// Whether the /rever Claude Code skill is installed globally. The install
// command itself is shown in the UI for the user to run in their own terminal.

function skillPath(): string {
  return join(homedir(), '.claude', 'skills', 'rever', 'SKILL.md')
}

export function skillInstalled(): boolean {
  return existsSync(skillPath())
}
