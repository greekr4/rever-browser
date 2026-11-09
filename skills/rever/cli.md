# revb — scripting a running Rever Browser

`revb` runs a Node script against the Rever Browser app. Every MCP tool the app
exposes becomes a global `async` function, so a multi-step job (navigate → grep →
extract, or a payload loop) runs in **one** Bash call instead of one LLM turn per
tool. Intermediate data (response bodies, bundles) stays in script variables —
print only the summary you need.

## Prerequisites

- The app must be **running and fully loaded** (first tab rendered). Right after
  launch, browser tools fail with `no active browser target` until the first
  page is attached — wait a few seconds and retry.
- Node 18+. No install step: `node bin/revb.mjs …` from the repo (or link it as `revb`).
- Run one app instance at a time — a second instance overwrites the published
  endpoint and `revb` talks to whichever started last.

## Commands

```bash
revb run script.mjs              # run a script file
revb run - <<'EOF'               # or a script from stdin
console.log((await list_requests({ limit: 20 })).length)
EOF
revb call get_request '{"requestId":"123.4"}'   # one call, prints the result
revb list                        # every tool: name<TAB>first sentence of its description
revb skill --full                # this guide + the live tool catalog of the running app
revb endpoint                    # url / pid / endpoint file (diagnostics)
```

`run` and `call` exit 1 on any thrown error, so `&&` chains and CI checks work.

## Script API

- **Every tool is a global**: `await browser_navigate({ url })`, `await list_requests({ limit: 50 })`.
  Names and arguments are exactly those in `revb list` / `revb skill --full` — nothing is renamed.
- `rev.tools()` → `[{ name, description }]` of the live catalog.
- `rev.call(name, args)` → call a tool by string name.
- `rev.endpoint` → the MCP URL in use.
- `sh(cmd, input?)` → run a shell command, returns its stdout (`sh('jq .id', JSON.stringify(x))`).
- Plain ESM: top-level `await`, `import fs from 'node:fs'`, etc. No sandbox — same trust as `node script.mjs`.

### What a call returns

- Text that is JSON → the parsed object/array (`list_requests` returns a **bare array**).
- Any other text (snapshots, extracted code) → a string.
- Results the server cuts at 12,000 chars (`[truncated: … tool_result_more …]`) are
  **paged automatically** — you always get the whole value.
- Image results (`browser_screenshot`) → a file path (`$REVER_SHOT_DIR` or the OS temp dir).
- Extra notes the app attaches (`[scope off] …`, auto-answered dialogs, started
  downloads) go to **stderr**, so stdout stays clean for `| jq`.
- Tool errors (`isError`) and protocol errors **throw**. Use `try/catch` to branch.

## Tool groups (run `revb skill --full` for the exact current list)

| Prefix / tools | Purpose |
|---|---|
| `browser_*`, `wait_*`, `dom_*`, `dialog_*` | drive the tab: navigate, snapshot (`rN` refs), click, type, evaluate, wait |
| `list_requests`, `get_request`, `fetch_body`, `har_*`, `ws_*` | captured traffic |
| `list_scripts`, `grep_script(s)`, `extract_context`, `deob*`, `detect_bundler` | JS bundle analysis |
| `wasm_*`, `list_wasm`, `grep_wasm` | WASM signing code |
| `crypto_*`, `hmac_compute`, `hash_iter`, `decode_token`, `auth_dump` | crypto / auth |
| `replay_request`, `repeater_send`, `burst_send`, `intruder_run` | resend and fuzz |
| `*_probe`, `scan_secrets`, `ui_crawl` | red-team probes (set scope first) |
| `intercept_*`, `override_*`, `inject_*`, `header_*` | modify traffic and pages |
| `console_*`, `cookie_*`, `local*`, `session*`, `sw_*` | page state |
| `finding_*`, `export_*` | record and export results |

## Examples (verified against `test-fixtures/api-target`)

**Reversing — find the signing code without dumping the bundle.** Pass `host`: the
traffic store holds scripts from every tab, not just the target.

```js
await browser_navigate({ url: 'https://target.example.com' })
await browser_wait_for({ text: 'Sign in', timeoutMs: 5000 })
const g = await grep_scripts({ pattern: 'hmac|sign|secret', host: 'target.example.com' })
for (const r of g.results.slice(0, 3)) {
  const ctx = await extract_context({ requestId: r.requestId, byteOffset: r.matches[0].offset, before: 200, after: 200 })
  console.log(r.url, '\n', ctx)
}
```

**Red team — payload loop on a captured request.** The probe replays a captured
request with each payload substituted at the `§` marker (captured as `%C2%A7`).

```js
await scope_set({ hosts: ['target.example.com'] })
await browser_evaluate({ expression: "fetch('/api/search?q=§').then(r => r.status)" })
const reqs = await list_requests({ limit: 200 })
const base = reqs.find(r => r.url.includes('/api/search') && r.url.includes('%C2%A7'))
const r = await payload_probe({ requestId: base.requestId, payloads: ["' OR 1=1--", '<svg/onload=alert(1)>', '{{7*7}}'] })
console.log(JSON.stringify(r.filter(x => x.reflectedRaw).map(x => x.payload)))
```

**Traffic map.**

```js
const reqs = await list_requests({ limit: 200 })   // bare array, limit ≤ 200
const api = reqs.filter(r => r.url.includes('/api/'))
console.table(api.map(r => ({ m: r.method, path: new URL(r.url).pathname, s: r.status })))
```

## Anti-patterns

- **Printing whole bodies or bundles.** `console.log(await get_request(...))` on a
  600 KB script puts it all back into your context. Filter, slice or count first.
- **One `revb call` per step in a loop from the agent.** That is the MCP round-trip
  cost again — put the loop in a script.
- **Reusing `rN` refs after an action.** Every click/type/navigate returns a fresh
  snapshot; old refs point at different elements. Re-read them from the result.
- **Red-team tools without `scope_set`.** Probes and replays are only fenced to
  the target when a scope is set; otherwise any host is allowed (stderr warns).
- **Retrying through an open page dialog.** While an `alert/confirm/prompt` the
  page raised on its own is open, every tool throws `waits for the user`. A
  `try/catch` cannot clear it — answer it in the app window, then rerun.

## Notes

- Every call shows in the app's activity overlay and on-page HUD, so the user
  sees what the script is doing.
- If the app reports a different minor version than this checkout, `revb` warns
  on stderr: tool names in this guide may have drifted — trust `revb list`.
- Each run opens one MCP session and closes it on exit (including Ctrl-C).
