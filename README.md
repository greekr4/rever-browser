# rever-browser

An AI-agent-driven desktop browser for reverse-engineering web APIs.

`rever-browser` is an Electron app that pairs a real Chromium tab with an ACP-based coding agent. You browse a target site in an embedded `<webview>`; the app captures every network request via the Chrome DevTools Protocol, and the agent can read that traffic, analyze the site's JavaScript bundles, and drive the tab itself through an in-process MCP tool server. The goal is to go from "what requests does this site make?" to "here is how to reproduce its API" without leaving the app.

## Features

- **Live traffic capture** — All `Network.*` events from the browsed tab are recorded into a ring buffer. Response bodies are fetched lazily and image/video/font/CSS payloads are skipped to keep the buffer lean.
- **AI agent chat** — Talk to a coding agent that sees the captured traffic and can act on the page. Claude Code is the default; Codex is also supported.
- **Browser automation** — The agent can navigate, click, type, scroll, screenshot, and take accessibility snapshots of the live tab.
- **Bundle analysis** — Grep, extract, detect the bundler for, and deobfuscate the JavaScript already captured in the traffic store (no re-download), including a `webcrack`-backed deobfuscator.
- **Deep API tooling** — A broad MCP tool set covering request repeater, intruder, header/override editing, HAR export, source-map recovery, crypto/decode helpers, WebSocket and service-worker inspection, and more.

## Requirements

- [Bun](https://bun.sh) (used as the package manager — not npm/pnpm)
- Node.js (for the ACP agent binaries below)
- **Agent binaries on your PATH:**
  - `claude-agent-acp` — required for the default Claude Code agent
    ```bash
    npm i -g @agentclientprotocol/claude-agent-acp
    ```
  - `codex-acp` — required for the Codex agent
    ```bash
    npm i -g @agentclientprotocol/codex-acp
    ```
- `webcrack` on your PATH (optional) — enables the `deobfuscate_script` tool

## Getting started

```bash
bun install      # install dependencies
bun run dev      # start electron-vite dev (main + preload + renderer with HMR)
```

Other commands:

```bash
bun run build      # production build to out/
bun run typecheck  # type-check with tsconfig.node.json + tsconfig.web.json
```

If HMR doesn't pick up a change to main- or preload-process code, kill the Electron process and re-run `bun run dev`:

```bash
pgrep -f "Electron|electron-vite" | xargs -r kill -9
```

## Usage

1. Run `bun run dev` to launch the app.
2. Enter a URL in the embedded browser and navigate to your target site.
3. Interact with the site — requests appear live in the traffic list as they happen.
4. Open the chat panel, pick an agent (Claude Code or Codex), and ask it about the captured traffic — for example, to explain an endpoint, reconstruct an auth flow, or generate client code that reproduces a request.
5. The agent reads the traffic store and drives the tab through MCP tools to answer.

## Architecture

Three Electron processes with strict separation; all cross-process work goes through preload IPC.

- **main** (`src/main/`) — Node + Electron APIs. Owns the `<webview>`'s CDP debugger, spawns ACP agent processes, and hosts the in-process HTTP MCP server the agent calls back into.
- **preload** (`src/preload/index.ts`) — The single source of truth for the renderer-visible surface, exposed as `window.rev` via `contextBridge`.
- **renderer** (`src/renderer/src/`) — React 19 + Vite. Hosts the `<webview>` tag and the chat UI.

### Data flows

**Traffic capture:** `webview Network.* events → main/chrome-cdp.ts → main/traffic-store.ts → renderer (TrafficList)`

**Agent loop:** `ChatPanel → ACPChatTransport → preload IPC → main/acp-session.ts → ACP agent child process → MCP tools → main/mcp/server.ts → tools read traffic-store / drive CDP`

The MCP server starts lazily on the first agent spawn and binds to a random localhost port. See `docs/` for additional design notes.

## License

[MIT](./LICENSE)
