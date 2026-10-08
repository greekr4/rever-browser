import { defineConfig } from '@playwright/test'

// Drives the real app: Electron launch → chat panel → live ACP agent → MCP
// tools against test-fixtures. Each chat turn is a real model call, so tests
// run serially with generous timeouts. Build first (`bun run e2e` does).
export default defineConfig({
  testDir: '.',
  testMatch: '**/*.spec.ts',
  timeout: 8 * 60_000,
  expect: { timeout: 15_000 },
  workers: 1,
  reporter: [['list']],
  globalSetup: './global-setup.ts',
  outputDir: '../test-results/e2e'
})
