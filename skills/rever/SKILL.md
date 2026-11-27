---
name: rever
description: Reverse-engineer web APIs by driving a running Rever Browser instance — connect to its published MCP endpoint and use its browser-automation, network-capture, and JS-bundle-analysis tools. Use when the user types /rever, asks to reverse or analyze a website's API, capture or inspect its network traffic, deobfuscate its JavaScript, or reproduce its requests with Rever Browser.
---

# Rever Browser

Drive a running **Rever Browser** app from this Claude Code session to reverse-engineer web APIs. Rever captures a real tab's network traffic and exposes ~140 MCP tools (browser control, traffic store, bundle analysis, interception, crypto tracing) that you use here.

## Prerequisite

The **Rever Browser app must be running** — it publishes its control socket on startup to:

```
~/Library/Application Support/rever-browser/mcp-endpoint.json
```

That file holds `socket` and `token`. The path is per-user, so always read it from the file. If the file is missing, tell the user to launch Rever Browser first.

Rever's tools are a **CLI**, not MCP server tools: every call goes through `revb` in the shell.

## Connect

```bash
revb list                                  # tool names
revb describe <tool>                       # one tool's argument schema
revb call <tool> '{"key": "value"}'        # call it
revb skill                                 # the full CLI guide
```

`revb` lives in the Rever Browser repo (`bin/revb.mjs`, zero dependencies, Node 18+). If it is not on PATH, call it by path: `node <repo>/bin/revb.mjs ...`.

The bundled helper does the same over the socket without Node (works immediately, reads the endpoint each call):

```bash
python3 rever.py --list
python3 rever.py browser_navigate '{"url":"https://example.com"}'
python3 rever.py list_requests
```

## Tool map

- **Browser** — `browser_navigate`, `browser_snapshot` (accessibility tree with `rN` refs), `browser_click`/`browser_type`/`browser_scroll` (by ref), `browser_screenshot`, `browser_evaluate`, `browser_wait_for`, `browser_tabs`.
- **Traffic** — `list_requests`, `get_request`, `get_request_initiator`, `fetch_body`, `har_export`.
- **Bundle analysis** — `list_scripts`, `grep_script`/`grep_scripts`, `extract_context`, `detect_bundler`, `deobfuscate_script` / `deob_auto`, `get_original_source`, `list_sources` (source maps).
- **Auth & crypto** — `auth_dump`, `decode_token`, `crypto_trace_start/stop/list`, `crypto_chain`, `hmac_compute`, `hash_iter`.
- **Reproduce & fuzz** — `find_api_base`, `graphql_introspect`, `burst_send`, `intruder_run`, `crlf_test`, `lfi_probe`, `create_macro` / `export_python_client`.
- **Intercept & override** — `intercept_add`/`intercept_fulfill`/`intercept_fail`, `inject_*`, `dom_*`.
- **Console / cookies** — `console_logs`, `console_eval`, `cookie_list`/`cookie_set`/`cookie_delete`.
- **Findings** — `finding_add`, `finding_list`, `finding_export`.

Not offered in this build (registered but hidden from every profile, because they cannot work here): `bp_*` breakpoints (a breakpoint freezes the tab), `exec_trace`, `inspect_listeners`, `header_preset_apply`/`header_preset_disable`, `console_get_props`, `console_exceptions`.

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

- **"No Rever Browser endpoint found"** → Rever Browser isn't running. Launch the app.
- **`revb: command not found`** → run it as `node <repo>/bin/revb.mjs ...`.
- **Connection refused / stale** → Rever restarted. The socket path is stable, but a dead pid in the endpoint file means the app is gone; launch it again.
