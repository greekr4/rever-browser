<div align="center">

<img src="./site/icon.png" alt="rever-browser" width="120" />

# rever-browser

### The AI browser for API reverse engineering.

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE)
[![Download](https://img.shields.io/github/v/release/greekr4/rever-browser?label=Download&color=8b5cf6)](https://github.com/greekr4/rever-browser/releases/latest)

**[🌐 Website](https://greekr4.github.io/rever-browser/)** / **[⬇ Download macOS](https://github.com/greekr4/rever-browser/releases/latest)** / **[⬇ Download Windows](https://github.com/greekr4/rever-browser/releases/latest)**

[한국어](./README.ko.md) · [Demo](#demo) · [What is it?](#what-is-rever-browser) · [Features](#features) · [Getting started](#getting-started) · [Architecture](#architecture)

</div>

---

## Demo

<div align="center">

[![rever-browser demo](https://greekr4.github.io/rever-browser/demo.gif)](https://greekr4.github.io/rever-browser/demo.mp4)

**Ask in plain English — the agent drives a real browser, finds API flaws, proves them live, and turns it into a one-click macro.**

[▶ Watch the full demo](https://greekr4.github.io/rever-browser/demo.mp4)

</div>

## What is rever-browser?

`rever-browser` is an Electron app that pairs a real Chromium tab with an ACP-based coding agent. You browse a target site in an embedded `<webview>`; the app captures every network request via the Chrome DevTools Protocol, and the agent can read that traffic, analyze the site's JavaScript bundles, and drive the tab itself through an in-process MCP tool server. The goal is to go from "what requests does this site make?" to "here is how to reproduce its API" without leaving the app.

## Features

- **Live traffic capture** — All `Network.*` events from the browsed tab are recorded into a ring buffer. Response bodies are fetched lazily and image/video/font/CSS payloads are skipped to keep the buffer lean.
- **AI agent chat** — Talk to a coding agent that sees the captured traffic and can act on the page. Claude Code is the default; Codex is also supported.
- **ACP or terminal mode** — Switch the agent pane between the structured ACP chat and a real terminal running the local Claude Code CLI, with rever's MCP server wired in automatically so the CLI agent gets the same browser/traffic tools.
- **Browser automation** — Accessibility snapshots with stable element refs, plus click, hover, type, scroll, drag, canvas draw, clipboard paste, file upload, key press, tab and viewport control, and screenshots of the live tab.
- **Bundle & script analysis** — Grep, extract, detect the bundler for, and deobfuscate the JavaScript already captured in the traffic store (no re-download) with a `webcrack`-backed deobfuscator; recover original TypeScript/JS via embedded source maps; and classify anti-bot "VM" bundles (Akamai / risk-control / captcha style) that flag when static deobfuscation is a dead end.

**A deep API-reversing tool surface — 129 MCP tools across 37 domains**, all driven by the agent (or from a shell). Highlights:

- **Repeater & Intruder** — Replay any captured request through the *real* browser context (its cookies, TLS, HTTP/2) so it behaves like the page itself. A Burp-Intruder-style marker fuzzer fires a payload list and tabulates status / length / timing / body-hash for blind boolean or timing-oracle enumeration. A manual Repeater panel is built into the app, too.
- **Interception & local overrides** — Proxy-style rules that log, block, or pause-and-modify requests (edit, fulfill, or fail a held request), plus DevTools-style Local Overrides that swap a response body — or an entire JS bundle — for a local copy.
- **Crypto & signature reversing** — Hook Web Crypto live to capture HMAC keys, signed messages, and the resulting signatures at call time (no debugger needed); compute and iterate HMAC/hashes; walk nested base64/hex/url/rot13 decode chains; decode JWTs and other tokens; look up PHP "magic hashes"; and decode opaque protobuf / gRPC-web bodies without a `.proto`.
- **Structure discovery** — GraphQL schema introspection to SDL, request-initiator call stacks that jump from a request to the `script:line` that issued it, API-base detection, request diffing, WebSocket frame capture, and service-worker / Cache API inspection.
- **Security audit** — Sweep captured traffic for embedded secrets (JWTs, cloud/API keys, private keys), analyze security headers, and run reflection/XSS, path-traversal/LFI, and CRLF probes — with a findings log you can export.
- **Export** — HAR 1.2 (loads into Burp Suite, Caido, or any HAR analyzer) and a ready-to-run Python (`requests` / `httpx`) client for any captured request.
- **Vision judge** — When the accessibility tree can't tell (a captcha, a modal overlay, whether a search actually ran), the agent screenshots the page and asks a vision model to judge what's visible.

- **Macros & workflows** — Turn a sequence of tool calls into a named macro that replays deterministically with no LLM in the loop; in-app Macro, Pipeline, and Template editors build these one-click flows.
- **Browser profiles** — Named persistent or incognito profiles, each an isolated cookie/storage jar. Open a tab under any profile from the tab bar, or create one seeded directly from a real browser profile by name.
- **Cookie import** — Pull a logged-in session into the active profile from Chrome, Edge, Brave, Arc, Chromium, Vivaldi, Firefox, or Safari (macOS), picking the source browser and profile by its display name.
- **Grab & markup** — Click any element on the page to capture its screenshot and context (selector, ref, tag, text): the context is dropped into the chat for the agent, and the screenshot opens in a markup editor for rectangles, arrows, and freehand notes, then copy to clipboard or save.

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

**Just want to use it?** Grab a build from the [latest release](https://github.com/greekr4/rever-browser/releases/latest) and install an agent binary — see [Requirements](#requirements):

- **macOS** (Apple Silicon or Intel `.dmg`) — drag it to Applications. Unsigned, so on first launch right-click the app → **Open**.
- **Windows** (`-setup.exe`) — run the installer. Unsigned, so click **More info → Run anyway** if SmartScreen warns.

**Building from source:**

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

## Use Rever from Claude Code (the `/rever` skill)

Rever ships a Claude Code skill so any `claude` session can drive a running Rever Browser and use its 129 MCP tools for API reversing. It's published as a standalone repo — [greekr4/rever-browser-skill](https://github.com/greekr4/rever-browser-skill) — installable with the [`skills`](https://www.npmjs.com/package/skills) CLI (no source checkout needed, so it works for DMG/EXE users):

```bash
npx skills add greekr4/rever-browser-skill --global --agent claude-code
```

**Use:** launch the Rever Browser app (it publishes its MCP endpoint on startup), then in any `claude` session type:

```
/rever
```

The skill resolves the app's endpoint and either registers it as native MCP tools (`claude mcp add --transport http rever …`) or calls tools directly via the bundled `rever.py`. macOS for now (the endpoint path is under `~/Library/Application Support/`). The in-repo copy under `skills/rever/` is the development source mirrored to that repo.

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

[Apache-2.0](./LICENSE) — see [NOTICE](./NOTICE) for attribution.

## Third-party licenses

All dependencies are listed in [`package.json`](./package.json); every package in the dependency tree uses a permissive license (MIT, Apache-2.0, ISC, or BSD) — no copyleft.

External tools invoked as separate processes (not bundled or distributed with this project): [claude-agent-acp](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp) (Apache-2.0), [codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp) (Apache-2.0), [webcrack](https://www.npmjs.com/package/webcrack) (MIT).

## Contributing

Bug reports, feature suggestions, and PRs are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md) for the workflow.
