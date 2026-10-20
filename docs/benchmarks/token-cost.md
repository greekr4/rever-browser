# Token-cost benchmark — Playwright MCP vs rever-browser

Both tools let an agent drive a real browser. The question is how many **input
tokens** the model has to ingest to reverse an API through each one's tools.

All counts are **measured**, not estimated — produced with
[`tiktoken`](https://github.com/openai/tiktoken) (`cl100k_base`, a close proxy
for current LLM tokenizers) over **real captured traffic** and a **real
minified bundle**. The gap is a property of the tool surface, not the exact
tokenizer: a different encoder shifts the absolute numbers a little and leaves
the ratio essentially unchanged.

The baseline is [Playwright MCP](https://github.com/microsoft/playwright-mcp)
(`@playwright/mcp`), the usual way to give an agent a browser. It has rich
navigation and a compact network list, but **no script-body search, no crypto
hooks, and no Repeater** — so any step that means reading JavaScript returns the
whole script.

## Target

The [`shop-demo`](../../test-fixtures/shop-demo) fixture (`127.0.0.1:8780`): a
storefront whose `GET /api/product/:id` call is signed with `HMAC-SHA256`, the
key embedded in its client bundle. Task: **reproduce one signed request** — find
the call, read its signature, find the signing code, replay it.

## Scenario A — the full task (shop-demo)

| Step | Playwright MCP | rever-browser |
|---|--:|--:|
| navigate + list network | 1,538 | 752 |
| request detail (headers / signature) | 448 | 735 |
| find the signing code | 4,167 | 656 |
| replay / forge | 212 | 212 |
| **Total** | **6,365** | **2,355** |

**→ 63% fewer tokens (2.7× less).**

Playwright's network *list* is actually tighter than rever's, and its
request *detail* is smaller too. rever pulls ahead at **"find the signing
code"**: Playwright returns the whole `app.js` (a 13 KB hand-written file here),
while rever's `grep_scripts` returns only the matching lines. On this tiny
fixture that is a 6× step difference — on a real bundle it is the whole game.

## Scenario B — the step that dominates on real targets

Finding the signing code in a **real 422 KB minified bundle**
(`primer-react-*.js`, captured live):

| | tokens |
|---|--:|
| Playwright MCP — returns the full script text | 178,244 |
| rever-browser — `grep_scripts` result | 819 |

**→ 99.5% fewer tokens (218× less).**

Playwright has no way to search a script body, so inspecting it means returning
all of it; the cost scales linearly with bundle size. `grep_scripts` returns
only the matching snippets (with byte offsets), so the cost stays flat no matter
how large the bundle is. The same holds for crypto analysis (`crypto_trace`
captures the key/signature at call time instead of returning the code that
computes it) and replay (`repeater_send` vs. hand-writing a `fetch`).

## Reproduce

With the app running and the fixture captured (see `docs/agent-testing.md`):

```bash
# --- rever-browser: focused tool outputs ---
python3 scripts/mcp-call.py list_requests '{"host":"127.0.0.1"}' > r_list.json
python3 scripts/mcp-call.py get_request   '{"requestId":"<id>"}' > r_get.json
python3 scripts/mcp-call.py grep_scripts  '{"pattern":"sign"}'   > r_grep.json
python3 scripts/mcp-call.py repeater_send '{"requestId":"<id>"}' > r_repeater.json
```

Playwright MCP side — drive `@playwright/mcp` to the same page and save each
tool result with its `filename` argument: `browser_navigate`, then
`browser_network_requests`, `browser_network_request` (the signed call),
`browser_evaluate` fetching the script text.

```bash
# count any artifact with the same tokenizer
uv run --with tiktoken python -c \
  "import tiktoken,sys;e=tiktoken.get_encoding('cl100k_base');print(len(e.encode(open(sys.argv[1]).read())))" \
  <file>
```
