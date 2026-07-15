import { BrowserWindow } from 'electron'

export type AiActionKind =
  | 'navigate'
  | 'click'
  | 'type'
  | 'scroll'
  | 'snapshot'
  | 'screenshot'
  | 'evaluate'
  | 'extract'

export interface AiAction {
  kind: AiActionKind
  label: string
  detail?: string
  ts: number
}

export function emitAiAction(action: Omit<AiAction, 'ts'>): void {
  const payload: AiAction = { ...action, ts: Date.now() }
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send('ai:action', payload)
  }
}
