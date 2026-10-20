<div align="center">

<img src="./docs/media/icon.png" alt="rever-browser" width="120" />

# rever-browser

### An agent-native workbench for API reverse engineering.

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](./LICENSE)
[![Download](https://img.shields.io/github/v/release/greekr4/rever-browser?label=Download&color=8b5cf6)](https://github.com/greekr4/rever-browser/releases/latest)

**[⬇ Download macOS](https://github.com/greekr4/rever-browser/releases/latest)** · **[⬇ Download Windows](https://github.com/greekr4/rever-browser/releases/latest)**

[한국어](./README.ko.md) · [What is it?](#what-is-rever-browser) · [How it works](#how-it-works) · [Features](#features) · [Tool surface](#tool-surface) · [Getting started](#getting-started) · [Architecture](#architecture)

</div>

---

<div align="center">

<img src="./docs/media/hero.gif" alt="The agent drives a real browser, captures the traffic, and shows its findings in the UI" width="860" />

</div>

## What is rever-browser?

`rever-browser` is an Electron desktop app that pairs a **real Chromium tab** with an **ACP coding agent** (Claude Code by default). You browse a target site in an embedded `<webview>`; the app records every network request through the Chrome DevTools Protocol, and the agent reads that traffic, analyzes the site's JavaScript and WASM, and drives the tab itself through an in-process **MCP tool server** with **156 tools across 44 domains**.

It takes you from *"what requests does this site make?"* to *"here's a one-click macro that reproduces its signed API"* without ever leaving the app, and you watch the agent do it step by step in the tab you're already looking at.

## How it works

Reversing an API in rever-browser is a five-phase loop. The agent moves through it on its own, and every step is visible in the UI.

| Phase | What happens | Example tools |
|---|---|---|
| **Capture** | Browse the target in the real tab. Every request lands in a live traffic list the agent can read. | `list_requests`, `get_request`, `browser_navigate` |
| **Analyze** | Grep the JS bundles, deobfuscate, read source maps, hook Web Crypto live, decode opaque bodies. | `grep_scripts`, `deobfuscate_script`, `crypto_trace_start`, `crypto_chain` |
| **Reproduce** | Replay a request through the browser's own context (its cookies, TLS, HTTP/2) and tamper with it until it works standalone. | `repeater_send`, `wasm_decompile`, `graphql_introspect` |
| **Verify** | Prove the flaw live: fuzz a marker, sweep for secrets, run XSS / path-traversal / CRLF probes, log findings. | `intruder_run`, `scan_secrets`, `path_probe`, `finding_add` |
| **Automate** | Freeze the working sequence into a deterministic macro, a CLI command, or a Python client, with no LLM in the loop. | `create_macro`, `cli_compile`, `export_python_client` |

<div align="center">

<img src="./docs/media/demo.gif" alt="Grab an element, mark it up, and have the agent analyze the request signing" width="860" />

*Grab any element → mark up the screenshot → the agent analyzes how the request is signed.*

<img src="./docs/media/repeater.gif" alt="Tamper a captured request in the Repeater, forge the signature, and reveal a hidden field" width="860" />

*Repeater: change a captured request, forge the leaked signature, and surface a field the UI never shows.*

<img src="./docs/media/macro.gif" alt="Ask the agent to build a multi-step macro, then save and replay it from the Workflows panel" width="860" />

*Macros: ask the agent to build a multi-step flow, then save it and replay it deterministically with no LLM in the loop.*

</div>

## Features

- **Live traffic capture.** Every `Network.*` event from the browsed tab is recorded into a ring buffer. Bodies are fetched lazily, and image/video/font/CSS payloads are skipped to keep it lean.
- **Pick your agent.** Five agents across four providers: Claude Code and Codex as local ACP binaries, plus Claude, OpenAI, and Grok driven directly through their APIs with your own key. Switch model from one unified picker.
- **ACP or terminal mode.** Run the structured ACP chat, or drop to a real terminal running the local `claude` CLI with rever's MCP server wired in automatically, so the CLI agent gets the same browser and traffic tools.
- **Browser automation.** Accessibility snapshots with stable element refs, plus click, hover, type, scroll, drag, canvas draw, clipboard paste, file upload, key press, tab and viewport control, and live screenshots.
- **Bundle, script & WASM analysis.** Grep, extract, detect the bundler, and deobfuscate captured JavaScript (no re-download) with a `webcrack`-backed deobfuscator; recover original TS/JS from source maps; flag anti-bot "VM" bundles; and disassemble `.wasm` modules to WAT, C-like, or full C. That last part closes the blind spot where request signing is compiled into WebAssembly.
- **Macros & workflows.** Turn a sequence of tool calls into a named macro that replays deterministically with no LLM in the loop. In-app Macro, Pipeline, and Template editors build these one-click flows, and any captured request exports to a ready-to-run CLI command or Python client.
- **Grab & markup.** Click any element to capture its screenshot and context (selector, ref, tag, text). The context drops into the chat and the screenshot opens in a markup editor for rectangles, arrows, and freehand notes.
- **Profiles & cookie import.** Named persistent or incognito profiles, each an isolated cookie/storage jar. Pull a logged-in session from Chrome, Edge, Brave, Arc, Chromium, Vivaldi, Firefox, or Safari.

## Token efficiency

An agent-driven browser is only useful if the work fits in a context window. rever's tools hand back **only the slice the agent needs**: a grep match instead of a whole bundle, a captured signature instead of the code that computes it. So a reversing task costs far fewer tokens than the same task run through [Playwright MCP](https://github.com/microsoft/playwright-mcp), the usual way to give an agent a browser.

<div align="center">

<img src="./docs/media/token-savings.svg" alt="Measured token cost: Playwright MCP vs rever-browser" width="820" />

</div>

| Task (measured, same agent flow) | Playwright MCP | rever-browser | |
|---|--:|--:|:--|
| Reproduce one signed request (shop-demo) | 6,365 | 2,355 | **2.7× less** |
| Find signing code in a real 422 KB bundle | 178,244 | 819 | **218× less** |

Playwright MCP has no script-body grep, crypto hooks, or Repeater, so analyzing JavaScript forces the whole bundle into context. rever's cost stays flat as the target grows. Counted with tiktoken (`cl100k`) on real captured traffic.

## Tool surface

**156 MCP tools across 44 domains**, all driven by the agent (or from a shell). By category:

| Category | What it covers |
|---|---|
| **Traffic & capture** | List/get requests, pagination, request diffing, body inspection, headers |
| **Browser control** | Accessibility snapshots, click/type/scroll/drag, DOM edits, dialogs, waits, scope, vision judge |
| **Scripts & bundles** | Grep/extract scripts, deobfuscate, source-map recovery, anti-bot bundle detection |
| **WASM** | Decompile to WAT / C, dump sections & symbols, grep strings, xref exports to JS |
| **Crypto & signatures** | Live Web Crypto hooks, HMAC/hash compute, base64/hex/JWT decode chains, protobuf/gRPC-web decode |
| **Structure discovery** | GraphQL introspection, request-initiator call stacks, listeners, exec trace, service workers, console |
| **Replay & fuzz** | Repeater (real browser context), Intruder marker fuzzer, bursts, payload probes |
| **Interception & overrides** | Log/block/pause-and-modify rules, local response & bundle overrides, script injection |
| **Security audit** | Secret sweep, security headers, XSS / path-traversal / CRLF probes, findings log, auth analysis |
| **Storage & realtime** | Cookie/storage jars, WebSocket frame capture |
| **Automation & export** | Macros, CLI compile, HAR 1.2 export, Python client export |

> A full per-tool reference lives in `docs/` and is also discoverable at runtime via the MCP endpoint (`python3 scripts/mcp-call.py --list`).

## Requirements

- [Bun](https://bun.sh): the package manager (not npm/pnpm)
- Node.js: for the ACP agent binaries below
- **Agent binaries on your PATH** (only for the ACP agents; the API agents just need a key):
  - `claude-agent-acp` for the default Claude Code agent: `npm i -g @agentclientprotocol/claude-agent-acp`
  - `codex-acp` for the Codex agent: `npm i -g @agentclientprotocol/codex-acp`
- `webcrack` on your PATH (optional) to enable the `deobfuscate_script` tool

## Getting started

**Just want to use it?** Grab a build from the [latest release](https://github.com/greekr4/rever-browser/releases/latest) and install an agent binary (see [Requirements](#requirements)):

- **macOS.** Pick the `.dmg` for your chip (`arm64` for Apple Silicon, `x64` for Intel), open it, and drag **Rever Browser** onto **Applications**.

  The build is unsigned, so Gatekeeper blocks the first launch. macOS may even say the app *"is damaged"*; that message is the quarantine flag, not a corrupt download. Clear it:

  ```bash
  xattr -dr com.apple.quarantine "/Applications/Rever Browser.app"
  ```

  Or launch once, let it be blocked, then open **System Settings → Privacy & Security → Open Anyway**. You only do this once per install.

- **Windows** (`-setup.exe`). Run the installer, then click **More info → Run anyway** if SmartScreen warns.

**Building from source:**

```bash
bun install      # install dependencies
bun run dev      # start electron-vite dev (main + preload + renderer with HMR)
```

Other commands:

```bash
bun run build      # production build to out/
bun run typecheck  # type-check with tsconfig.node.json + tsconfig.web.json
bun run test       # vitest unit tests
```

If HMR doesn't pick up a change to main- or preload-process code, kill Electron and re-run `bun run dev`:

```bash
pgrep -f "Electron|electron-vite" | xargs -r kill -9
```

## Use Rever from Claude Code (the `/rever` skill)

Rever ships a Claude Code skill so any `claude` session can drive a running Rever Browser and use its MCP tools. It lives in its own repo, [greekr4/rever-browser-skill](https://github.com/greekr4/rever-browser-skill), installable with the [`skills`](https://www.npmjs.com/package/skills) CLI (no source checkout, so it works for DMG/EXE users):

```bash
npx skills add greekr4/rever-browser-skill --global --agent claude-code
```

Launch the Rever Browser app (it publishes its MCP endpoint on startup), then in any `claude` session type `/rever`. The skill resolves the endpoint and either registers it as native MCP tools or calls them via the bundled `rever.py`. It's macOS only for now, since the endpoint path lives under `~/Library/Application Support/`.

## Architecture

Three Electron processes with strict separation; all cross-process work goes through preload IPC.

- **main** (`src/main/`) runs Node and Electron APIs. It owns the `<webview>`'s CDP debugger, spawns ACP agent processes, and hosts the in-process HTTP MCP server the agent calls back into.
- **preload** (`src/preload/index.ts`) is the single source of truth for the renderer-visible surface, exposed as `window.rev` via `contextBridge`.
- **renderer** (`src/renderer/src/`) is React 19 + Vite. It hosts the `<webview>` tag and the chat UI.

**Traffic capture:** `webview Network.* events → main/chrome-cdp.ts → main/traffic-store.ts → renderer (TrafficList)`

**Agent loop:** `ChatPanel → ACPChatTransport → preload IPC → main/acp-session.ts → ACP agent child process → MCP tools → main/mcp/server.ts → tools read traffic-store / drive CDP`

The MCP server starts lazily on the first agent spawn and binds to a random localhost port. See `docs/` for design notes.

## License

[MIT](./LICENSE). See [NOTICE](./NOTICE) for attribution.

Every package in the dependency tree uses a permissive license (MIT, Apache-2.0, ISC, or BSD), none copyleft. External tools invoked as separate processes (not bundled): [claude-agent-acp](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp) (Apache-2.0), [codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp) (Apache-2.0), [webcrack](https://www.npmjs.com/package/webcrack) (MIT).

## Contributing

Bug reports, feature suggestions, and PRs are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md).

## Responsible use

rever-browser is a tool for **authorized** security research, API debugging, and interoperability work: testing systems you own or have explicit permission to test. Reproducing, tampering with, or probing an API you are not authorized to touch may violate a site's terms of service and the law. You are responsible for how you use it.
