#!/usr/bin/env python3
"""Call a running Rever Browser's MCP tools from Claude Code.

Rever Browser publishes its MCP endpoint on startup (OS-assigned port), so this
reads the published address and talks to it. Requires the Rever Browser app to
be running.

Usage:
    python3 rever.py --url                     # print the current MCP URL
    python3 rever.py --list                    # list available tools
    python3 rever.py <tool> '<json args>'      # call a tool

Examples:
    python3 rever.py browser_navigate '{"url":"https://example.com"}'
    python3 rever.py browser_snapshot '{"full":true}'
    python3 rever.py list_requests
"""
import base64
import json
import os
import sys
import tempfile
import time
import urllib.request

ENDPOINT = os.path.expanduser(
    "~/Library/Application Support/rever-browser/mcp-endpoint.json"
)


def find_url() -> str:
    try:
        with open(ENDPOINT) as f:
            return json.load(f)["url"]
    except (OSError, KeyError, json.JSONDecodeError):
        raise SystemExit(
            "No Rever Browser MCP endpoint found. Launch the Rever Browser app, "
            "then try again."
        )


class Client:
    def __init__(self, url: str):
        self.url = url
        self.sid = None
        self._rpc(1, "initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "rever-skill", "version": "1"},
        })
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _post(self, payload: dict) -> str:
        req = urllib.request.Request(
            self.url,
            data=json.dumps(payload).encode(),
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
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
        for line in raw.splitlines():
            if line.startswith("data: "):
                return json.loads(line[6:])
        return json.loads(raw) if raw.strip() else {}

    def tools(self) -> list:
        res = self._rpc(2, "tools/list", {})
        return [t["name"] for t in res.get("result", {}).get("tools", [])]

    def call(self, name: str, args: dict) -> str:
        res = self._rpc(3, "tools/call", {"name": name, "arguments": args})
        if "error" in res:
            return f"ERROR: {res['error']}"
        out = []
        for c in res.get("result", {}).get("content", []):
            if c.get("type") == "text":
                out.append(c.get("text", ""))
            elif c.get("type") == "image":
                out.append(f"[image saved: {self._save_image(c)}]")
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
    if sys.argv[1] == "--url":
        print(find_url())
        return
    client = Client(find_url())
    if sys.argv[1] in ("--list", "-l"):
        print("\n".join(sorted(client.tools())))
        return
    args = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
    print(client.call(sys.argv[1], args))


if __name__ == "__main__":
    main()
