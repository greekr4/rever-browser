// scriptId → URL for every script the attached tabs have parsed, fed from
// Debugger.scriptParsed in chrome-cdp. CDP reports listener and function
// locations by scriptId only, so tools that point at code (inspect_listeners,
// execution traces) need this to say *which file* a location is in.
//
// Keyed by webContents + CDP session + scriptId: script ids are per isolate,
// so two tabs (or a page and its out-of-process frame) reuse the same ids.

const MAX_SCRIPTS = 20_000

const scripts = new Map<string, string>()

function key(wcId: number, sessionId: string | undefined, scriptId: string): string {
  return `${wcId}:${sessionId ?? ''}:${scriptId}`
}

export function recordScript(
  wcId: number,
  sessionId: string | undefined,
  scriptId: string,
  url: string
): void {
  const k = key(wcId, sessionId, scriptId)
  scripts.delete(k)
  scripts.set(k, url)
  while (scripts.size > MAX_SCRIPTS) {
    const oldest = scripts.keys().next().value
    if (oldest === undefined) break
    scripts.delete(oldest)
  }
}

// null for unknown ids and for scripts with no URL (inline / eval).
export function scriptUrl(
  wcId: number,
  sessionId: string | undefined,
  scriptId: string
): string | null {
  return scripts.get(key(wcId, sessionId, scriptId)) || null
}

export function clearScriptsFor(wcId: number): void {
  const prefix = `${wcId}:`
  for (const k of [...scripts.keys()]) if (k.startsWith(prefix)) scripts.delete(k)
}

export function __resetScriptRegistryForTests(): void {
  scripts.clear()
}
