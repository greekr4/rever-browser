import { app } from 'electron'
import { spawn, type ChildProcess } from 'node:child_process'
import { existsSync, readFileSync, writeFileSync, unlinkSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { createServer } from 'node:net'

const CHROME_PATHS_MACOS = [
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium'
]

let chromeProcess: ChildProcess | null = null
let chromePort: number | null = null
let beforeQuitRegistered = false

function getPidFilePath(): string {
  return join(app.getPath('userData'), 'external-chrome.pid')
}

async function findFreePort(startPort = 9222): Promise<number> {
  for (let port = startPort; port < startPort + 20; port++) {
    const free = await new Promise<boolean>((resolve) => {
      const server = createServer()
      server.once('error', () => resolve(false))
      server.once('listening', () => {
        server.close(() => resolve(true))
      })
      server.listen(port, '127.0.0.1')
    })
    if (free) return port
  }
  throw new Error('No free port found in range')
}

function findChromeBinary(): string {
  for (const p of CHROME_PATHS_MACOS) {
    if (existsSync(p)) return p
  }
  throw new Error(
    'Google Chrome not found. Install Chrome at /Applications/Google Chrome.app or Chromium at /Applications/Chromium.app'
  )
}

async function pollUntilReady(port: number, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://localhost:${port}/json/version`)
      if (res.ok) return
    } catch {
      // not ready yet
    }
    await new Promise((r) => setTimeout(r, 200))
  }
  throw new Error(`Chrome CDP not reachable on port ${port} after ${timeoutMs}ms`)
}

export async function launchExternalChrome(): Promise<{ port: number; pid: number }> {
  // Check for stale PID
  const pidFile = getPidFilePath()
  if (existsSync(pidFile)) {
    try {
      const stalePid = parseInt(readFileSync(pidFile, 'utf8').trim(), 10)
      if (!isNaN(stalePid)) {
        try {
          process.kill(stalePid, 0)
          // Still alive — reuse if port is known
          if (chromePort !== null) {
            return { port: chromePort, pid: stalePid }
          }
        } catch {
          // Dead process — clean up
          unlinkSync(pidFile)
        }
      }
    } catch {
      // ignore read errors
    }
  }

  const chromePath = findChromeBinary()
  const port = await findFreePort(9222)
  const profileDir = join(app.getPath('userData'), 'external-chrome-profile')
  mkdirSync(profileDir, { recursive: true })

  const args = [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profileDir}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--window-position=10000,10000',
    '--window-size=1280,800',
    '--disable-features=Translate'
  ]

  chromeProcess = spawn(chromePath, args, {
    detached: false,
    stdio: 'ignore'
  })

  const pid = chromeProcess.pid!
  writeFileSync(pidFile, String(pid), 'utf8')
  chromePort = port

  chromeProcess.on('exit', () => {
    chromeProcess = null
    chromePort = null
    try {
      if (existsSync(pidFile)) unlinkSync(pidFile)
    } catch {}
  })

  if (!beforeQuitRegistered) {
    beforeQuitRegistered = true
    app.on('before-quit', () => {
      void killExternalChrome()
    })
  }

  await pollUntilReady(port)
  return { port, pid }
}

export async function killExternalChrome(): Promise<void> {
  if (chromeProcess) {
    try {
      chromeProcess.kill('SIGTERM')
    } catch {}
    chromeProcess = null
  }
  chromePort = null
  const pidFile = getPidFilePath()
  try {
    if (existsSync(pidFile)) unlinkSync(pidFile)
  } catch {}
}

export function isExternalChromeRunning(): boolean {
  if (!chromeProcess) return false
  try {
    process.kill(chromeProcess.pid!, 0)
    return true
  } catch {
    return false
  }
}
