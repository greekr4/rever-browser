import { ipcMain } from 'electron'

import { spawnTerminal, writeTerminal, resizeTerminal, killTerminal } from '../terminal'

// Local CLI-agent terminal IPC (node-pty). Extracted from index.ts.
export function registerTerminalIpc(): void {
  ipcMain.handle(
    'terminal:spawn',
    (event, opts: { cols: number; rows: number; agent: 'claude' | 'shell' }) =>
      spawnTerminal(
        opts,
        (id, data) => event.sender.send(`terminal:data:${id}`, data),
        (id, code) => event.sender.send(`terminal:exit:${id}`, code)
      )
  )
  ipcMain.on('terminal:input', (_e, id: string, data: string) => writeTerminal(id, data))
  ipcMain.on('terminal:resize', (_e, id: string, cols: number, rows: number) =>
    resizeTerminal(id, cols, rows)
  )
  ipcMain.on('terminal:kill', (_e, id: string) => killTerminal(id))
}
