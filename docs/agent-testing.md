# Testing browser tools without a human

An agent working on this repo can drive the running app itself. Do that instead
of writing test steps for the user to run and paste back — the loop is faster,
and it removes the transcription gap where a real failure gets summarised away.

## Why this works

`src/main/index.ts` starts the MCP server when the app becomes ready, and
`src/main/mcp/server.ts` publishes its address to:

```
~/Library/Application Support/rever-browser/mcp-endpoint.json
```

The port is OS-assigned, so that file is the only way in from outside. The
server used to start lazily on the first agent spawn, which meant the in-app
chat panel was the sole entry point.

## The loop

```bash
# 1. Start the app. Main- and preload-process changes need a full restart;
#    HMR does not pick them up.
pgrep -f "Electron|electron-vite" | xargs -r kill -9
bun run dev > /tmp/rever-dev.log 2>&1 &

# 2. Wait for the endpoint to appear (about a second).
E="$HOME/Library/Application Support/rever-browser/mcp-endpoint.json"
for i in $(seq 1 40); do [ -f "$E" ] && break; sleep 1; done

# 3. Drive it.
python3 scripts/mcp-call.py browser_navigate '{"url":"https://example.com"}'
python3 scripts/mcp-call.py browser_snapshot '{"full":true}'
python3 scripts/mcp-call.py browser_click '{"ref":"r4"}'
python3 scripts/mcp-call.py list_requests
python3 scripts/mcp-call.py --list          # every registered tool
```

Any tool in `src/main/mcp/tools/` is reachable this way, not just the browser
ones — `list_requests`, `grep_scripts`, `repeater_send`, and the rest all work.

## Fixtures

`test-fixtures/` holds pages whose expected result is printed on the page next
to each group, so checking a run is mechanical rather than a judgement call.
Serve them on two origins — the second one exists so cross-origin and cross-site
frames can be told apart:

```bash
cd test-fixtures
python3 -m http.server 8777 --bind 127.0.0.1 &
python3 -m http.server 8778 --bind 0.0.0.0 &
```

| Fixture | URL | Covers |
|---|---|---|
| `snapshot-fixture.html` | `http://127.0.0.1:8777/snapshot-fixture.html` | viewport filtering, hidden/occluded nodes, off-screen scroll hints, click-scan detection and over-detection guards |
| `iframe-fixture.html` | `http://127.0.0.1:8777/iframe-fixture.html` | same-origin, cross-origin and cross-site (OOPIF) frames |
| `oopif-demo.html` | `http://127.0.0.1:8777/oopif-demo.html` | realistic OOPIF case: fake checkout with a same-origin coupon widget (visible) vs a cross-site payment widget (`localhost:8778`, pre-patch invisible — P1–P3 and the SECRET string must not appear) |
| `shadow-fixture.html` | `http://127.0.0.1:8777/shadow-fixture.html` | open/closed/nested shadow roots, inner scroll containers, `*new` node marking |
| `api-target/` (see below) | `http://127.0.0.1:8779/` | the API-analysis tools — traffic capture, scripts, sourcemaps, crypto, replay/repeater, fuzz probes, WebSocket, storage |
| `api-target/.../wasm-target.html` | `http://127.0.0.1:8779/wasm-target.html` | `list_wasm` / `wasm_decompile` — loads `/sign.wasm` (export `checksum`); SW-precached, so a reload serves it from cache and exercises the `refetchBody` binary-body fix |

The API target is a Bun server, not a static page, because it needs to sign
requests, upgrade WebSockets, and register a service worker. Every secret it
uses is printed in the file header, so a tool's answer is checkable:

```bash
bun test-fixtures/api-target/server.ts   # listens on 8779
# rebuild the bundle + source map after editing src/:
cd test-fixtures/api-target && bun build src/app.ts --outdir public --minify --sourcemap=linked --entry-naming app.js
# rebuild the WASM fixture after editing src/checksum.wat (uses the wabt JS API — no extra CLI):
bun -e 'const w=await (await import("wabt")).default();const fs=await import("node:fs");const m=w.parseWat("checksum.wat",fs.readFileSync("test-fixtures/api-target/src/checksum.wat","utf8"));fs.writeFileSync("test-fixtures/api-target/public/sign.wasm",Buffer.from(m.toBinary({}).buffer));m.destroy()'
```

Its service worker caches `app.js`, so a second load serves the bundle from
cache — which is exactly the path that exercises the response-refetch fallback
in `chrome-cdp.ts`. The `§` fuzz marker is captured percent-encoded (`%C2%A7`);
the fuzz tools decode it, so a captured `?name=§` request is a valid base.

Clicking a fixture target fires a distinctly-named `fetch`, so `list_requests`
proves a trusted click actually reached the handler. That matters: a click with
a wrong coordinate offset does not error, it silently hits something else.

## When the app is not enough

Some questions are about CDP itself rather than about this app — "does
`Accessibility.getFullAXTree({frameId})` return content for a cross-origin
frame?", "do `DOMSnapshot` bounds share a coordinate space with
`getBoundingClientRect`?". Drive a real Chrome directly for those:

```js
// node probe.mjs — chrome-remote-interface is already a dependency
import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
const CDP = createRequire(process.cwd() + '/').call(null, 'chrome-remote-interface')

spawn('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  '--remote-debugging-port=9333', '--headless=new', '--no-first-run',
  '--user-data-dir=/tmp/cdp-probe', 'about:blank'
], { stdio: 'ignore' })
```

Answering the CDP question first has repeatedly turned out cheaper than
implementing against an assumption and debugging the result.

## Gotchas

- **Refs go stale after every action.** `browser_click`, `browser_type` and
  `browser_navigate` all return a fresh snapshot; ref numbers from before the
  action point at different elements afterwards. Re-read them from the response.
- **Main-process edits need a restart.** Renderer changes hot-reload; anything
  under `src/main/` does not.
- **A silent miss is the failure mode to design for.** Prefer a check that
  proves the effect (a captured request, a changed count) over one that merely
  shows no error.

## Interaction coverage

`interaction-fixture.html` holds one interaction per group and prints PASS only
when the real event reached the page, so a tool that fakes it cannot score. It
is how the keyboard, right-click, double-click and multi-select gaps were
found — each was invisible until a page demanded it.

All eighteen groups pass. Three want their own run, because they change the
page out from under the others: **L** follows a link and reloads, **P** grows
the DOM past the click-scan cap, and **R** needs the canvas scrolled into view
before it is scanned.

Two of them need the call order to be right rather than a special tool:

- **J, hover-revealed menu.** Hover the visible trigger, not the wrapper's box.
  The hover survives the snapshot that follows, so the revealed item can be
  read and clicked normally.
- **B and H.** `browser_press_key` returns a fresh snapshot, so a ref read
  before the press is stale by the time of the next one. Re-read it each time.

- **M and N.** `browser_handle_dialog` arms a ONE-SHOT answer, so call it
  immediately before the click that opens the dialog rather than earlier.
- **P.** Growing a list past the click-scan cap disables the scan for the whole
  page, and every role-less click target disappears with it. The snapshot now
  says so; before that it was silent, and a canvas that had a ref a moment
  earlier simply stopped appearing.

`browser_drag` reports which mechanism ran — `native` uses the browser's own
drag machinery, `pointer` a held-button gesture for dnd-kit / SortableJS, and
`synthetic` dispatches DragEvents with `isTrusted=false` as a last resort. Only
the last is detectable by a page that checks, so prefer to confirm the drop
changed something rather than trusting the call.

## Known limitations (deferred)

- **Click-scan does not reach inside frames.** A `<div onClick>` with no ARIA
  role gets a ref in the top document only; inside any frame it is invisible,
  because the scan needs `getEventListeners` and an isolated world does not
  have it. Framed elements therefore need a real role to be clickable by ref.
- **No viewport pruning inside an out-of-process frame.** The layout pass runs
  in the page's renderer and that frame's backend ids belong to another
  process, where the same number means a different element. Its nodes are
  emitted unfiltered rather than filtered against the wrong boxes.

## OOPIF support (implemented)

An out-of-process iframe — one whose src is a different *site* (different
eTLD+1; port and subdomain do not count) — runs in its own renderer, so the
page's CDP session cannot read it. `Target.setAutoAttach({flatten: true})` in
`chrome-cdp.ts` registers a session per such frame (`mcp/oopif.ts`), and
`collectOopifFrames` splices their trees into the snapshot under the owning
`<iframe>`.

Every command about one of those nodes carries the frame's `sessionId`. That
is not an optimisation: a probe against real Chrome showed the page session
answering `DOM.resolveNode` for a foreign backendNodeId with a **different
node** instead of an error, so an unrouted click lands somewhere else and
nothing reports a failure. `Network.enable` runs per OOPIF session too —
without it only the frame's document load is captured, not the API calls the
widget makes, which are the reason to look inside it at all.

Check it with `oopif-demo.html`: P1–P3 must carry refs, and clicking P3 must
show `GET /oopif-pay-clicked?card=...` in `list_requests` — the request proves
the click reached that button rather than a coordinate-adjacent one.
