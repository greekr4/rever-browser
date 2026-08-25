# WASM Analysis

> 한국어: [`wasm-analysis.ko.md`](./wasm-analysis.ko.md)

Read WebAssembly the way you read JavaScript. When a site computes request
signatures inside a `.wasm` module (compiled from Rust / C / Go / AssemblyScript),
the `crypto_trace` tool — which only sees the **Web Crypto API** — is blind. This
feature closes that gap: it turns already-captured `.wasm` bodies into readable
text and links their exports back to the JS that calls them.

No re-download and no capture path of its own — it reads the `.wasm` bytes the
CDP layer already stored in `traffic-store`.

## Tools

All five are MCP tools; drive them from a shell with `scripts/mcp-call.py`.

| Tool | What it does |
|---|---|
| `list_wasm` | List captured modules (`application/wasm` bodies or `.wasm` URLs), largest first. Gives you the `requestId` the other tools need. |
| `wasm_decompile` | Disassemble one module to text. `format:"wat"` (default) / `"decompile"` (C-like) / `"c"` (full C source via wasm2c). |
| `wasm_info` | Module summary via `wasm-objdump -x`: types, imports, exports, and real function/symbol names when a name section is present. |
| `grep_wasm` | A `strings`-style scan of the body — algorithm ids, embedded keys, URLs, symbol names. Optional regex filter. |
| `wasm_xref` | Bridge WASM ↔ JS: for each export, grep the captured scripts for its name and show where it is called. |

### `wasm_decompile` output tiers

Same bytes, three readability levels:

- **`wat`** — the guaranteed baseline. Runs **in-process** via the `wabt` JS API,
  so it is always available once `wabt` is installed. No subprocess, no temp file.
- **`decompile`** — a higher-level, C-like view (`wasm-decompile`).
- **`c`** — full C source (`wasm2c`); the most readable for complex crypto/signing
  routines.

`decompile` and `c` shell out to bundled `wabt` binaries; if that path is
unavailable they **degrade gracefully to WAT** rather than erroring.

> `full:true` is kept as a deprecated alias for `format:"decompile"`.

## Quick start

```bash
# 1. load the page that fetches the .wasm (its bytes get captured)
python3 scripts/mcp-call.py browser_navigate '{"url":"https://example.com/app"}'

# 2. find the module
python3 scripts/mcp-call.py list_wasm
#   -> [{ "requestId": "123.4", "url": ".../sign.wasm", "bytes": 4096, ... }]

# 3. map its surface first
python3 scripts/mcp-call.py wasm_info   '{"requestId":"123.4"}'

# 4. read the logic (pick a tier)
python3 scripts/mcp-call.py wasm_decompile '{"requestId":"123.4","format":"c"}'

# 5. skim for constants, and find the JS call site
python3 scripts/mcp-call.py grep_wasm '{"requestId":"123.4","pattern":"HMAC|sign|key"}'
python3 scripts/mcp-call.py wasm_xref  '{"requestId":"123.4"}'
```

## The capture fix that makes it work

Normally CDP hands us the response body and we keep its base64 flag intact, so
`.wasm` capture already works. But when a body is served **from cache or a
service worker**, CDP has no copy and the app re-fetches the URL. That fallback
(`refetchBody` in `chrome-cdp.ts`) used to force every body to UTF-8 — which
mangles the `\0asm` magic and every non-UTF-8 byte, leaving a cache-delivered
`.wasm` undecodable.

The fix: `encodeRefetchedBody` base64-encodes binary MIME types
(`application/wasm`, `application/octet-stream`, image/video/audio/font) and
leaves text as UTF-8. Real sites serving their `.wasm` from a service-worker
cache now decode correctly.

## Dependency: `wabt`

`wabt` is an **npm devDependency**, not a PATH binary:

```bash
bun add -d wabt
```

- WAT runs through the in-process JS API.
- `decompile` / `c` / `wasm_info` spawn `node_modules/wabt/bin/*` with
  Electron-as-node (`ELECTRON_RUN_AS_NODE=1`), so no external `node` is needed.
- "wabt missing" therefore means a failed `import('wabt')`, not an ENOENT — the
  tools return the hint `bun add -d wabt` and never crash.

Limits on the subprocess path: 30,000 ms timeout and a 5,242,880-byte output cap.

## Internals

- `src/main/mcp/tools/wasm.ts` — the five MCP tool registrations.
- `src/main/mcp/wasm-analysis.ts` — pure, electron-free helpers (so they run
  under vitest): `encodeRefetchedBody`, `listWasm`, `getWasmBuffer`, `wasmToWat`,
  `runWabtBin` + `wasmDecompileFull` / `wasmToC` / `wasmObjdump`,
  `extractWasmStrings`, `parseWatExports`, `xrefExports`, `decompileRequest`.
- `src/main/chrome-cdp.ts` — imports `encodeRefetchedBody` for the `refetchBody`
  fix.

## Testing

- Unit: `bun run test` (`src/main/mcp/wasm-analysis.test.ts`).
- Live: serve the fixture and drive the tools — see
  [`agent-testing.md`](./agent-testing.md). The `wasm-target.html` fixture
  (port 8779) loads `/sign.wasm` (export `checksum`) via an external
  `/wasm-caller.js`, and is service-worker-precached so a reload exercises the
  `refetchBody` fix.

## Scope

Phase 1 is read/analyze only. Deliberately out of scope: symbol recovery beyond
the name section, deeper WASM↔JS dataflow, and `crypto_trace` integration flags.
