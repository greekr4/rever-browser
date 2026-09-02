#!/usr/bin/env python3
"""Call the running app's MCP tools from outside the app.

The app publishes its endpoint on startup (see src/main/mcp/server.ts), which is
what makes this possible at all: the port is OS-assigned, so without the
published address there is no way in except the in-app chat panel.

Usage:
    python3 scripts/mcp-call.py <tool> ['<json args>']
    python3 scripts/mcp-call.py --list

Examples:
    python3 scripts/mcp-call.py browser_navigate '{"url":"https://example.com"}'
    python3 scripts/mcp-call.py browser_snapshot '{"full":true}'
    python3 scripts/mcp-call.py browser_click '{"ref":"r4"}'
    python3 scripts/mcp-call.py list_requests
"""
import base64
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.request

ENDPOINT = os.path.expanduser(
    "~/Library/Application Support/rever-browser/mcp-endpoint.json"
)


def find_endpoint() -> tuple[str, str | None]:
    """Published (url, token) first; fall back to sniffing the listening port.

    The sniffed fallback has no token, so it only works against an app build
    that predates bearer auth — kept for that case, otherwise expect a 401.
    """
    try:
        with open(ENDPOINT) as f:
            data = json.load(f)
            return data["url"], data.get("token")
    except (OSError, KeyError, json.JSONDecodeError):
        pass

    out = subprocess.run(
        ["lsof", "-nP", "-iTCP", "-sTCP:LISTEN"], capture_output=True, text=True
    ).stdout
    for line in out.splitlines():
        if line.startswith("Electron") and "127.0.0.1:" in line:
            m = re.search(r"127\.0\.0\.1:(\d+)", line)
            if m:
                return f"http://127.0.0.1:{m.group(1)}/mcp", None

    raise SystemExit(
        "No MCP endpoint found. Start the app with `bun run dev` and try again."
    )


class Client:
    def __init__(self, url: str, token: str | None = None):
        self.url = url
        self.token = token
        self.sid = None
        self._rpc(1, "initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "mcp-call", "version": "1"},
        })
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _post(self, payload: dict) -> str:
        req = urllib.request.Request(
            self.url,
            data=json.dumps(payload).encode(),
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
                **({"Authorization": f"Bearer {self.token}"} if self.token else {}),
                **({"mcp-session-id": self.sid} if self.sid else {}),
            },
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=180) as r:
            if self.sid is None:
                self.sid = r.headers.get("mcp-session-id")
            return r.read().decode()

    def _rpc(self, rid: int, method: str, params: dict) -> dict:
        raw = self._post({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        # Streamable HTTP replies as SSE; the payload rides on a `data:` line.
        for line in raw.splitlines():
            if line.startswith("data: "):
                return json.loads(line[6:])
        return json.loads(raw) if raw.strip() else {}

    def tools(self) -> list[str]:
        res = self._rpc(2, "tools/list", {})
        return [t["name"] for t in res.get("result", {}).get("tools", [])]

    def call(self, name: str, args: dict) -> str:
        res = self._rpc(3, "tools/call", {"name": name, "arguments": args})
        if "error" in res:
            return f"ERROR: {res['error']}"
        content = res.get("result", {}).get("content", [])
        out = []
        for c in content:
            if c.get("type") == "text":
                out.append(c.get("text", ""))
            elif c.get("type") == "image":
                # Images were dropped entirely, so browser_screenshot printed
                # nothing and there was no way to look at the page from here.
                path = self._save_image(c)
                out.append(f"[image saved: {path}]")
        return "\n".join(out)

    @staticmethod
    def _save_image(part: dict) -> str:
        ext = {"image/png": "png", "image/jpeg": "jpg"}.get(part.get("mimeType", ""), "bin")
        out_dir = os.environ.get("REVER_SHOT_DIR", tempfile.gettempdir())
        os.makedirs(out_dir, exist_ok=True)
        path = os.path.join(out_dir, f"rever-shot-{int(time.time() * 1000)}.{ext}")
        with open(path, "wb") as f:
            f.write(base64.b64decode(part.get("data", "")))
        return path


def main() -> None:
    if len(sys.argv) < 2:
        print(__doc__.strip())
        raise SystemExit(1)

    client = Client(*find_endpoint())

    if sys.argv[1] in ("--list", "-l"):
        print("\n".join(sorted(client.tools())))
        return

    args = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
    print(client.call(sys.argv[1], args))


if __name__ == "__main__":
    main()
