import { advertisedNames, type ToolProfile } from '../mcp/tool-profile'

export function directToolRefusal(name: string): string {
  return `refused ${name}: not in the current tool profile or this session's prior calls.`
}

/** Names this completion may call. `dispatched` is only names already passed to `mcp.callTool`. */
export function beginDirectTurn(profile: ToolProfile, dispatched: ReadonlySet<string>): Set<string> {
  return new Set(advertisedNames(profile, dispatched))
}

/**
 * Refuse a name outside `allowed` without recording it.
 * A name that will be passed to `mcp.callTool` is added first, including when that call throws.
 */
export function takeDirectTool(
  name: string,
  allowed: ReadonlySet<string>,
  dispatched: Set<string>
): { run: true } | { run: false; content: string } {
  if (!allowed.has(name)) return { run: false, content: directToolRefusal(name) }
  dispatched.add(name)
  return { run: true }
}
