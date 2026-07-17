# Feature Plan — Workflows + Per-Tab Proxy

> Status: proposal / not yet implemented. Design notes for two new features plus a
> shortlist of follow-up candidates. Written against `main` @ `60476bc`.

## Context

`rever-browser` today pairs one Chromium `<webview>` (shared session `persist:rever`)
with an ACP/in-process agent that drives the tab through an in-process MCP tool server.
Two capability gaps limit real reverse-engineering work:

1. **No per-tab network routing.** Every tab shares one Electron `session`, so you can't
   put tab A behind proxy X and tab B behind proxy Y (or direct). Comparing a target
   across regions/IPs, or isolating a noisy target, is impossible without restarting.
2. **No reusable procedures.** Every investigation is retyped from scratch. The agent can
   do a login→capture→fuzz sequence once, but there's no way to *save* and *re-run* it.
   Chat history is the only persistence (localStorage, `rev:chat-history`).

This plan adds **per-tab proxy with full session isolation** and a **modular Workflow
subsystem** (three independent, individually-removable modules), and lists further
candidates.

---

## Feature 1 — Per-Tab Proxy (full isolation)

### Decision
Each tab gets **its own persistent partition + session** from birth
(`persist:rever-<tabId>`). Proxy is applied per-session via `session.setProxy()` and can
be changed live (no webview remount). Cookies/storage become **per-tab** — this is the
accepted trade-off (chosen: "탭별 완전 격리"). The Chrome-import / cookie-persistence flows
become partition-aware so per-tab seeding still works.

### Why this shape
- Electron proxy is **per-session**, not per-webContents. Simultaneous distinct proxies
  therefore *require* distinct sessions → distinct partitions → distinct cookie jars.
- `<webview partition>` is fixed at mount, but `session.setProxy()` is mutable at runtime.
  Giving each tab its own partition from the start means proxy toggling never needs a
  remount/reload.

### Key files & changes
- **`src/renderer/src/stores/tabs.ts`** — extend `Tab` with a proxy field:
  ```ts
  proxy?: { enabled: boolean; scheme: 'http'|'https'|'socks5'; host: string; port: number; auth?: { username: string } }
  ```
  (Password is NOT stored here — see auth note.) Add `setTabProxy(id, cfg)` action.
- **`src/renderer/src/components/WebviewTab.tsx:194`** — replace the hardcoded
  `partition="persist:rever"` with `partition={`persist:rever-${tab.id}`}`. Partition is
  keyed off the stable tab id, so it's set once at mount and never changes.
- **`src/main/index.ts`** — introduce a lazy session factory instead of the single
  `revSession` at `index.ts:216`:
  ```ts
  const tabSessions = new Map<string, Electron.Session>()
  function getTabSession(tabId: string) {
    const key = `persist:rever-${tabId}`
    let ses = tabSessions.get(tabId)
    if (!ses) {
      ses = session.fromPartition(key)
      ses.setPermissionRequestHandler((_wc, _p, cb) => cb(false)) // keep current deny-all
      ses.setPermissionCheckHandler(() => false)
      tabSessions.set(tabId, ses)
    }
    return ses
  }
  ```
  The default first tab (`t1`) partition is created the same way — the existing deny-all
  permission behavior is preserved by applying the same handlers per session.
- **New IPC** (`src/main/index.ts` + `src/preload/index.ts` as `window.rev.proxy`):
  - `proxy:set` `(tabId, cfg)` → `getTabSession(tabId).setProxy(rules)` where
    `rules = { proxyRules: `${scheme}://${host}:${port}` , proxyBypassRules: '<local>' }`;
    `cfg.enabled === false` → `setProxy({ mode: 'direct' })`.
  - `proxy:test` `(cfg)` (optional) → quick reachability probe via a throwaway session +
    `net.request` through the proxy.
- **Proxy auth (407):** `setProxy` rules can't carry credentials. Add an `app.on('login',
  (e, wc, authInfo, cb) => …)` handler in `src/main/index.ts`: when `authInfo.isProxy`,
  look up the tab owning `wc` and reply with its stored credentials. Store proxy passwords
  encrypted in main via the existing **`src/main/settings.ts`** `safeStorage` pattern
  (new `proxy-<tabId>-cred.bin`), never in the renderer/localStorage.
- **`src/main/cookie-persistence.ts` / `src/main/chrome-cookie-import.ts`** — these hardcode
  `PARTITION = 'persist:rever'`. Make them take a partition/tabId argument so import and
  snapshot target the active tab's session. On tab creation, optionally seed the new
  partition from the persisted cookie snapshot so "import my Chrome session, then browse"
  still works per tab.
- **Cleanup:** on `closeTab`, drop the `tabSessions` entry and clear its stored creds. (No
  need to destroy the on-disk partition unless we add an explicit "wipe tab data".)

### UI
Add a small proxy control reachable from the tab (gear on the address bar / tab context
menu) that opens a compact form (enable toggle, scheme, host, port, optional user/pass) +
a status dot (direct / proxied / error). Follows existing inline-popover styling with CSS
tokens from `styles.css`. No new bottom panel required.

### Verification
1. `bun run dev`. Open two tabs. Set tab A to a known local proxy (e.g. `http://127.0.0.1:8080`
   with mitmproxy/Burp listening), leave tab B direct.
2. Browse an IP-echo site (e.g. `httpbin.org/ip`, `ifconfig.me`) in both — tab A shows the
   proxy egress IP / appears in the proxy's log, tab B shows the direct IP.
3. Toggle tab A proxy off → next navigation goes direct, no reload glitch.
4. Authenticated proxy: confirm the `login` handler answers the 407 without a popup.
5. Confirm cookies set in tab A are NOT visible in tab B (isolation holds).

---

## Feature 2 — Workflow subsystem (modular, disposable)

### Decision
Build **all three** workflow forms, each as an **independent module that can be removed on
its own** (chosen: "세 개 다 별도로 구현하고 폐기 가능하도록"):

- **M1 — Macro (record & replay):** capture a sequence of tool calls (from an agent run or
  hand-authored), save it as a named recipe, replay it **deterministically without an LLM**,
  with parameter substitution (`{{var}}`) and simple conditionals.
- **M2 — Prompt templates:** reusable prompt/checklist snippets that kick off the agent
  (injected into the chat input). No auto-execution.
- **M3 — Visual pipeline:** node-graph editor to compose steps with branching/loops, run on
  the same executor as M1.

### Modular / disposable architecture
A thin **shared core** + **pluggable module registry** so any module is removed by deleting
its folder and one registry line — the other two keep working.

```
src/renderer/src/workflows/
  core/
    store.ts            # zustand persist `rev:workflows` — kind-tagged records
    registry.ts         # WorkflowKind[] registry (id, label, icon, Editor, Runner, run())
    WorkflowPanel.tsx   # host panel; lists items, dispatches to the kind's Editor/Runner
    types.ts            # shared Workflow / WorkflowStep / WorkflowRun types
  macro/     index.ts   # registers kind 'macro'  (M1)  — self-contained
  template/  index.ts   # registers kind 'template' (M2) — self-contained
  pipeline/  index.ts   # registers kind 'pipeline' (M3) — self-contained
```
- Each module default-exports a `WorkflowKind` and self-registers via `registry.register()`
  in an index barrel. **Removing a module = delete its folder + its one `import` line.** The
  panel iterates `registry.list()`, so the UI degrades gracefully.
- All three persist through the same `rev:workflows` store, discriminated by `kind`, so the
  store schema is shared but each module owns its `steps`/`body` shape.

### Deterministic executor (shared, main process)
The load-bearing reuse: **`getMcpBridge()` already exists** in
`src/main/providers/anthropic-provider.ts:47` (and a twin in `openai-provider.ts`) — it
connects an MCP `Client` to the local HTTP MCP server and lists tools.

- **Extract it** to `src/main/mcp/bridge.ts` (`getMcpBridge()` returning `{ client, tools }`),
  and have both providers import from there (removes current duplication).
- Add **`src/main/workflow-executor.ts`**: given a saved macro/pipeline, iterate steps and
  call `client.callTool({ name, arguments })` directly — **no model in the loop**, so replays
  are fast and deterministic. Emit progress via a `workflow:progress` IPC event (mirrors the
  `ai:action` overlay pattern). `agent-prompt` steps (in pipelines) route through
  **`promptSession()`** in `src/main/agent-router.ts` instead of `callTool`.
- Tool-call steps operate on the **active tab's CDP target** (existing single-active-target
  model in `chrome-cdp.ts`), so a macro drives whichever tab is focused — consistent with how
  the agent's browser tools already behave.

### Recording macros (M1)
Tool executions are already surfaced as `ai:action` events (see `AiActionOverlay`). Add an
opt-in "record" toggle that appends each executed `{ tool, input }` to a draft macro. Because
in-process providers call `client.callTool` in one place, the recorder can tap that call site
(or the `ai:action` emit) with no per-tool changes.

### Module specifics
- **M1 Macro** — Editor: step list (reorder/edit/delete, mark params as `{{var}}`). Runner:
  "Run" → `workflow:run` IPC → `workflow-executor`. Results per step shown inline.
- **M2 Template** — Editor: name + prompt text (with `{{var}}` prompts filled at use). Runner:
  "Use" pushes text into the chat via the existing **`chat-draft`** store
  (`src/renderer/src/stores/chat-draft.ts` → `push(text)`); user reviews and sends. Zero main-
  process code — fully removable.
- **M3 Pipeline** — Editor: lightweight node graph (start → steps → cond/loop). Compiles to
  the same `WorkflowStep[]` the executor runs, so it shares M1's runtime. Heaviest module;
  ships last.

### Key files
- New: `src/main/mcp/bridge.ts`, `src/main/workflow-executor.ts`,
  `src/renderer/src/workflows/**`.
- Edit: `src/main/providers/anthropic-provider.ts` & `openai-provider.ts` (import shared
  bridge), `src/preload/index.ts` (`window.rev.workflows.{list,save,delete,run,cancel}` +
  `onProgress`), `src/main/index.ts` (register workflow IPC),
  `src/renderer/src/components/FloatingChips.tsx` (add `'workflows'` to `PanelId` + a chip +
  render `<WorkflowPanel/>` — the established bottom-panel pattern),
  `src/renderer/src/stores/chat-draft.ts` (reused by M2, no change expected).

### Verification
1. **M2 first** (cheapest end-to-end): create a template, click Use, confirm text lands in
   chat input. Remove the `template/` folder + registry line → app still builds, other kinds
   unaffected (proves disposability).
2. **M1:** hand-author a 3-step macro (`browser_navigate` → `browser_type_selector` →
   `browser_click_selector`) against a local test page; Run; confirm each step executes on the
   active tab and progress events render. Then record a macro from a live agent run and replay
   it.
3. **M3:** build a 2-branch pipeline (`list_requests` → cond on status → `intruder_run`) and
   run it through the shared executor.
4. `bun run typecheck` clean; targeted `vitest` for `workflow-executor` step sequencing /
   `{{var}}` substitution (follow `traffic-store.test.ts`).

---

## Feature 3 — Additional candidates (shortlist, prioritized)

Ranked by value/effort for this RE-browser. Several extend existing MCP tools.

| # | Feature | What / why | Reuses |
|---|---------|-----------|--------|
| P1 | **cURL / code export** | One-click export a captured request or repeater spec as `curl` / `fetch` / Python `requests`. Core RE deliverable ("reproduce this API"). | `repeater.ts`, `traffic-store.ts`, `TrafficDetailDrawer` |
| P2 | **Proxy presets manager** | Named upstream proxies (label + scheme/host/port/creds); quick-assign to any tab. Direct complement to Feature 1. | `settings.ts` (encrypted), Feature 1 UI |
| P3 | **Match & Replace rules** | Persisted global find/replace on request/response headers+body (regex). Classic intercepting-proxy power feature. | `override.ts`, `headers.ts`, `intercept.ts` |
| P4 | **Scope control** | In-scope host allowlist; capture/tools focus on scope, mute the rest. Cuts noise on busy targets. | `traffic-store.ts`, `chrome-cdp.ts` filters |
| P5 | **Saved traffic filters / bookmarks** | Persist filter presets and star/tag individual requests; survives reload. | `traffic.ts` store, `TrafficList` |
| P6 | **Findings report export** | Export the existing `findings` tool set to a Markdown/HTML report. Turns a session into a shareable artifact. | `findings.ts`, `har.ts` |
| P7 | **Tab/session profiles** | Save & restore a set of tabs (URL + proxy + viewport) as a named workspace. Pairs with per-tab proxy + isolation. | `tabs.ts`, Feature 1 |

Recommended pickups after the two headline features: **P1** and **P2** (highest value,
lowest effort, and P2 directly amplifies Feature 1).

---

## Suggested build order

1. **Feature 1 — Per-tab proxy** (self-contained; unblocks P2/P7). Ship session factory +
   `setProxy` IPC + minimal UI first; add auth (407) and cookie-partition seeding second.
2. **Workflow core + M2 (templates)** — smallest slice that proves the modular registry and
   disposability end-to-end with near-zero main-process risk.
3. **Extract `mcp/bridge.ts` + `workflow-executor.ts`, then M1 (macro)** — the deterministic
   replay engine; the highest-value workflow module.
4. **M3 (visual pipeline)** — on top of the M1 runtime once it's proven.
5. **P1 (code export) + P2 (proxy presets)** as fast follow-ups.
