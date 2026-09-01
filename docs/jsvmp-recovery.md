# JSVMP / risk-control VM recovery

`detect_antibot_vm` tells you *that* a script is a custom opcode VM (Akamai / 风控 /
captcha style) and that static deobfuscation is a dead end. This doc is the *next
step*: how to actually recover the token/signature such a VM produces, using
rever-browser's own tools — no Selenium, no external headless driver, because the
app already drives a real webview.

Adapted from the `dsl-vm-reverse` methodology in
[reverse-skill](https://github.com/zhaoxuya520/reverse-skill) (MIT).

## When you're here

`detect_antibot_vm` returned `likely-vm` (score ≥ 5), or you see the signature by eye:

| Signal | What it looks like |
|---|---|
| IIFE + single-letter vars mapped to numbers | `!function(){var U=void 0,y=parseInt,E0=Function,...}` |
| Interpreter main loop | a function with `for(...) switch(d[7]&31){case 0:...}` — `&31` decodes an opcode |
| Constant table | repeated `C[9][123]` — function/string table lookups |
| Bound-call pattern | `W(C[idx],null,...)` where `W = Function.prototype.call.bind(call)` |
| Big single-line bundle, <1% zero bytes | 500KB+ of pure JS (not embedded WASM) |

If the file starts with `\x00asm` or has >20% zero bytes it's real WASM — use the
`wasm_*` tools (`list_wasm` / `wasm_decompile`), not this doc.

## The rule: don't fight the VM statically

Opcode/const-table extraction below is only to *understand* the flow enough to
place a hook. The token is bound to browser context (JA3, IP, cookies, headers),
so a pure-protocol reimplementation almost never validates server-side. **Recover
the value at runtime, in the real page** — that's what rever-browser is for.

## Phase 1 — Scope the VM statically (understand, don't reimplement)

Work off the body already captured in `traffic-store` (no re-download).

1. **Confirm & locate** — `list_scripts` (biggest first) → `detect_antibot_vm({requestId})`.
2. **Const/var map** — `grep_script({requestId, pattern: "var \\w+=\\d+"})` on the first
   few KB to recover the number-name aliases (`E=15,l=10,...`).
3. **Opcode set** — `grep_script({requestId, pattern: "case \\d+:"})`, then
   `extract_context` around each hit to classify it (BRANCH `d[7]=`, CALL `W(C[`,
   RETURN `return`/`throw`, ALLOC `new`, ARITH/STORE otherwise).
4. **Const table** — `grep_script({requestId, pattern: "C\\[9\\]\\[(\\d+)\\]"})` to find the
   function/string table and its index range.
5. **Export entry** — `grep_scripts` across bundles for the registration call
   (`register(`, `_modules`, the exported name like `getToken`). The exported
   function is usually stored as bytecode in the const table, reached via
   `register() → W(C[idx],...) → interpreter`.

Goal of Phase 1: know **which page action triggers the token** and **what the
exported function is called**. Then stop — you have enough to hook.

## Phase 2 — Capture the value at runtime (the payoff)

Prefer the lightest hook that yields the generated value. In order:

1. **Web Crypto path** — if the VM ends up calling `crypto.subtle` (sign/digest/encrypt):
   `crypto_trace_start` → trigger the page action → `crypto_trace_list`. Recovers the
   real key, message, and signature directly. Done.
2. **Wrap the builder** — `inject_add` a snippet (host glob for the target) that wraps
   the export or the registration sink and logs its return value:
   ```js
   // runs on page load; adjust the reachable path to the real global
   const g = window.AWSCInner || window.__vm__;
   if (g && g.register) {
     const orig = g.register.bind(g);
     g.register = (name, mod, factory) => orig(name, mod, () => {
       const api = factory();
       for (const k of Object.keys(api || {})) {
         if (typeof api[k] === 'function') {
           const f = api[k];
           api[k] = (...a) => { const r = f(...a); console.log('[vm]', k, JSON.stringify(r)); return r; };
         }
       }
       return api;
     });
   }
   ```
   Then trigger the action and read `console_logs`. Use `console_eval` to poke the
   global directly (`AWSCInner._modules['fy'].getToken({})`) once you know the path.
3. **Confirm the token flows to the wire** — `list_requests({since})` → `get_request`
   on the request that carries the token; `request_diff` two captures to see which
   param is the VM output.

**Do NOT reach for `bp_*`.** Breakpoints are a no-op in this build — execution never
pauses under Electron's `webContents.debugger`. Instrumentation (`crypto_trace` /
`inject_add` / `console_eval`) is the only path, not a fallback.

## Phase 3 — Reproduce & record

- Reproduce the signed call with `repeater_send` (browser context, keeps the live
  session) or `export_python_client`. Because the token is context-bound, a Node/
  Python client will usually need to *fetch a fresh token from the page* rather than
  recompute it — say so explicitly in the deliverable.
- Record it with `finding_add`: `category:"auth"`, the triggering `requestId` and the
  hook as `evidenceIds`, and the exact `reproCommand`. Mark `validated` only with ≥2
  independent evidence (e.g. the `crypto_trace` capture **and** the on-wire request).

## Opcode reference (from an observed AWSC-style VM)

Indicative only — every VM renumbers. Use it to speed up classification in Phase 1.

| Opcode | Type | Shape |
|---|---|---|
| 0, 8 | BRANCH | `d[7]=612` / `d[7]=d[k]?512:425` |
| 1, 13, 24 | CALL | `W(C[idx],null,...)` |
| 2, 3, 5, 14 | ARITH | `d[8]=d[4]-d[8]`, compares, string concat |
| 4, 12, 21 | STORE | `d[8]=d[5] in d[4]`, property get/set |
| 6, 15 | RETURN | `return gV` / `throw` |
| 7, 10, 16, 17 | ALLOC | array/local/stack setup |
| 9, 11, 22 | STRING/REGEX | `new fh("\\s",d[5])` |
| 19, 23, 25 | EXCEPTION | `try{...}catch` + branch |

Common result codes: `0` = pass (take sessionId + sig), `300` = blocked,
`8778`/`8776` = retry (too fast / failed), `69634` = generic failure.
