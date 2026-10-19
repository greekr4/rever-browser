import { chmodSync, readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'

import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { app, BrowserWindow } from 'electron'

import {
  TOOL_PROFILE_PROBE_REJECTED,
  advertisedNames,
  applyFollowCatalog,
  readToolProfile,
  type ToolProfile,
  type ToolToggle
} from './tool-profile'

export type { ToolToggle } from './tool-profile'

export type ProfileSource = 'user' | 'agent'

let loaded = false
let current: ToolProfile = 'browse'

const followServers = new Map<McpServer, Map<string, ToolToggle>>()

function notifyRenderers(profile: ToolProfile): void {
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send('tool-profile:changed', profile)
  }
}

function toolProfileFilePath(): string {
  return path.join(app.getPath('userData'), 'tool-profile.json')
}

function writeProfileFile(profile: ToolProfile): void {
  const file = toolProfileFilePath()
  writeFileSync(file, JSON.stringify({ version: 1, profile }, null, 2), { mode: 0o600 })
  // `mode` only applies when the file is created; an existing file keeps its old bits.
  chmodSync(file, 0o600)
}

function ensureLoaded(): void {
  if (loaded) return
  loaded = true
  let parsed: ToolProfile | null = null
  try {
    parsed = readToolProfile(readFileSync(toolProfileFilePath(), 'utf8'))
  } catch {
    parsed = null
  }
  if (parsed) {
    current = parsed
    return
  }
  current = 'browse'
  try {
    writeProfileFile('browse')
  } catch (e) {
    console.warn('[tool-profile] could not write tool-profile.json:', e)
  }
}

export function getToolProfile(): ToolProfile {
  ensureLoaded()
  return current
}

export function setToolProfile(
  profile: ToolProfile,
  source: ProfileSource
): { ok: true; profile: ToolProfile; advertised: number } | { ok: false; error: string } {
  if (source === 'agent' && profile === 'probe') {
    return { ok: false, error: TOOL_PROFILE_PROBE_REJECTED }
  }
  ensureLoaded()
  current = profile
  try {
    writeProfileFile(profile)
  } catch (e) {
    console.warn('[tool-profile] could not write tool-profile.json:', e)
  }
  for (const [mcp, handles] of followServers) {
    try {
      applyFollowCatalog(mcp, handles, current)
    } catch (e) {
      console.warn('[tool-profile] follow catalog apply failed:', e)
    }
  }
  notifyRenderers(current)
  return { ok: true, profile, advertised: advertisedNames(profile).length }
}

export function registerFollowServer(mcp: McpServer, handles: Map<string, ToolToggle>): void {
  followServers.set(mcp, handles)
}

export function unregisterFollowServer(mcp: McpServer): void {
  followServers.delete(mcp)
}
