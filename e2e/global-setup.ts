import { spawn, type ChildProcess } from 'node:child_process'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// Starts the api-target fixture (port 8779) unless one is already running,
// and returns a teardown that stops only the server this run started.
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const API_TARGET = 'http://127.0.0.1:8779/'

async function up(): Promise<boolean> {
  try {
    return (await fetch(API_TARGET)).ok
  } catch {
    return false
  }
}

export default async function globalSetup(): Promise<() => void> {
  if (await up()) return () => {}
  const server: ChildProcess = spawn('bun', ['test-fixtures/api-target/server.ts'], {
    cwd: ROOT,
    stdio: 'ignore'
  })
  for (let i = 0; i < 50 && !(await up()); i++) await new Promise((r) => setTimeout(r, 200))
  if (!(await up())) throw new Error('api-target fixture did not start on :8779')
  return () => {
    server.kill()
  }
}
