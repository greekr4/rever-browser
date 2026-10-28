import { app } from 'electron'
import { appendFile, mkdir } from 'node:fs/promises'
import { join } from 'node:path'

import type { SessionNotification } from '@agentclientprotocol/sdk'

import { redact, redactDeep } from './mcp/secret-scan'

// Append-only transcript of every prompt + streamed update, one JSON object per
// line (jsonl), per session. The ACP (Claude Code) agent keeps its own jsonl;
// direct providers (GLM/Grok/OpenAI/Anthropic) did not — this captures all of
// them uniformly in a place the app controls. Secrets/PII are masked first.
function logDir(): string {
  return join(app.getPath('userData'), 'session-logs')
}

function fileFor(sessionId: string): string {
  // Session ids carry provider prefixes like "glm:" — keep the path flat/safe.
  return join(logDir(), `${sessionId.replace(/[^A-Za-z0-9._-]/g, '_')}.jsonl`)
}

let dirReady: Promise<void> | null = null
async function ensureDir(): Promise<void> {
  if (!dirReady) dirReady = mkdir(logDir(), { recursive: true }).then(() => undefined)
  return dirReady
}

async function append(sessionId: string, entry: Record<string, unknown>): Promise<void> {
  try {
    await ensureDir()
    const line = JSON.stringify({ ts: new Date().toISOString(), sessionId, ...entry }) + '\n'
    await appendFile(fileFor(sessionId), line, 'utf8')
  } catch {
    // Logging is best-effort; never break a turn because the disk write failed.
  }
}

export function logPrompt(sessionId: string, text: string): void {
  void append(sessionId, { dir: 'prompt', text: redact(text) })
}

export function logUpdate(sessionId: string, notification: SessionNotification): void {
  void append(sessionId, { dir: 'update', update: redactDeep(notification) })
}

export function logStop(sessionId: string, stopReason: string): void {
  void append(sessionId, { dir: 'stop', stopReason })
}
