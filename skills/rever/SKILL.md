---
name: rever
description: Reverse-engineer web APIs by driving a running Rever Browser instance — connect to its published MCP endpoint and use its browser-automation, network-capture, and JS-bundle-analysis tools. Use when the user types /rever, asks to reverse or analyze a website's API, capture or inspect its network traffic, deobfuscate its JavaScript, or reproduce its requests with Rever Browser.
---

# Rever Browser

Drive a running **Rever Browser** app from this Claude Code session to reverse-engineer web APIs. Rever captures a real Chromium tab's network traffic and exposes ~140 MCP tools (browser control, traffic store, bundle analysis, interception, crypto tracing) that you use here.

## Prerequisite

The **Rever Browser app must be running** — it publishes its MCP endpoint on startup to:

```
~/Library/Application Support/rever-browser/mcp-endpoint.json
```

The port is OS-assigned (changes each launch), so always resolve it from that file. If the file is missing, tell the user to launch Rever Browser first.

## Connect

**Preferred — register as native MCP tools** (tools appear as `mcp__rever__*`):

```bash
claude mcp add --transport http rever "$(python3 -c "import json,os;print(json.load(open(os.path.expanduser('~/Library/Application Support/rever-browser/mcp-endpoint.json')))['url'])")"
```

Re-run this whenever Rever Browser restarts (the port changes). MCP servers load at session start, so if the tools aren't visible yet, start a fresh `claude` session.

**Quick calls without registration** — use the bundled helper (works immediately, reads the endpoint each call):

```bash
python3 "$(dirname "$0")/rever.py" --list                              # list tools
python3 rever.py browser_navigate '{"url":"https://example.com"}'
python3 rever.py list_requests
```

## Tool map

- **Browser** — `browser_navigate`, `browser_snapshot` (accessibility tree with `rN` refs), `browser_click`/`browser_type`/`browser_scroll` (by ref), `browser_screenshot`, `browser_evaluate`, `browser_wait_for`, `browser_tabs`.
- **Traffic** — `list_requests`, `get_request`, `get_request_initiator`, `fetch_body`, `har_export`.
- **Bundle analysis** — `list_scripts`, `grep_script`/`grep_scripts`, `extract_context`, `detect_bundler`, `deobfuscate_script` / `deob_auto`, `get_original_source`, `list_sources` (source maps).
- **Auth & crypto** — `auth_dump`, `decode_token`, `crypto_trace_start/stop/list`, `crypto_chain`, `hmac_compute`, `hash_iter`.
- **Reproduce & fuzz** — `find_api_base`, `graphql_introspect`, `burst_send`, `intruder_run`, `crlf_test`, `lfi_probe`, `create_macro` / `export_python_client`.
- **Intercept & override** — `intercept_add`/`intercept_fulfill`/`intercept_fail`, `inject_*`, `header_preset_*`, `dom_*`.
- **Debugger** — `bp_add`, `bp_eval_in_frame`, `bp_step_*`, `bp_resume`.
- **Console / cookies** — `console_logs`, `console_eval`, `console_exceptions`, `cookie_list`/`cookie_set`/`cookie_delete`.
- **Findings** — `finding_add`, `finding_list`, `finding_export`.

Run `--list` (or `tools/list`) for the full, current set.

## Workflow

1. **Navigate & observe** — `browser_navigate` to the target, then `browser_snapshot` to see actionable elements (`rN` refs).
2. **Reproduce the user action** — `browser_click` / `browser_type` on the refs to trigger the API call you care about.
3. **Find the request** — `list_requests` (filter by URL/method), then `get_request` for full headers/body and `get_request_initiator` for the call site.
4. **Understand the client** — `find_api_base`, `grep_scripts` for the endpoint/signing logic, `deobfuscate_script` on the relevant bundle, `crypto_trace_start` to catch signing/HMAC.
5. **Reproduce standalone** — confirm with `burst_send`, then `create_macro` / `export_python_client` to emit a runnable client.
6. **Record findings** — `finding_add` as you confirm each, `finding_export` at the end.

Prefer the traffic store over re-downloading: script bodies are already captured, so `grep_scripts` and `deobfuscate_script` work without re-fetching.

## Troubleshooting

- **"No MCP endpoint found"** → Rever Browser isn't running. Launch the app.
- **Tools missing after `claude mcp add`** → MCP loads at session start; open a new `claude` session, or use the `rever.py` helper for the current one.
- **Connection refused / stale** → Rever restarted and the port changed. Re-run the connect command.
