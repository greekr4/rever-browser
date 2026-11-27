#!/usr/bin/env python3
"""Call a running Rever Browser's tools from Claude Code.

Rever Browser publishes a control socket on startup, so this reads the
published path and talks to it. Requires the Rever Browser app to be running.

Usage:
    python3 rever.py --socket                  # print the current socket path
    python3 rever.py --list                    # list available tools
    python3 rever.py <tool> '<json args>'      # call a tool

Examples:
    python3 rever.py browser_navigate '{"url":"https://example.com"}'
    python3 rever.py browser_snapshot '{"full":true}'
    python3 rever.py list_requests
"""
import json
import os
import socket
import sys

ENDPOINT = os.path.expanduser(
    "~/Library/Application Support/rever-browser/control-endpoint.json"
)


def find_endpoint() -> dict:
    try:
        with open(ENDPOINT) as f:
            endpoint = json.load(f)
    except (OSError, json.JSONDecodeError):
        raise SystemExit(
            "No Rever Browser endpoint found. Launch the Rever Browser app, "
            "then try again."
        )
    # `socket` is the CLI's way in; an older app publishes only an HTTP `url`.
    path = endpoint.get("socket")
    if not path:
        raise SystemExit(
            "This Rever Browser build publishes no control socket. Update the app."
        )
    return endpoint


class Client:
    def __init__(self, endpoint: dict):
        self.token = endpoint["token"]
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(600)  # a tool may wait on the page for minutes
        self.sock.connect(endpoint["socket"])
        self.reader = self.sock.makefile("r", encoding="utf-8")

    def request(self, tool: str, args) -> dict:
        line = json.dumps({"token": self.token, "tool": tool, "args": args})
        self.sock.sendall((line + "\n").encode())
        reply = self.reader.readline()
        if not reply:
            raise SystemExit("The app closed the control socket.")
        return json.loads(reply)

    def tools(self) -> list:
        reply = self.request("__catalog", None)
        if not reply.get("ok"):
            raise SystemExit(f"ERROR: {reply.get('error')}")
        return [t["name"] for t in reply["result"]["tools"]]

    def call(self, name: str, args: dict) -> str:
        reply = self.request(name, args)
        if not reply.get("ok"):
            return f"ERROR: {reply.get('error')}"
        out = []
        for block in reply["result"].get("content", []):
            if block.get("type") == "text":
                out.append(block.get("text", ""))
        return "\n".join(out)

    def close(self) -> None:
        self.reader.close()
        self.sock.close()


def main() -> None:
    if len(sys.argv) < 2:
        print(__doc__.strip())
        raise SystemExit(1)
    endpoint = find_endpoint()
    if sys.argv[1] == "--socket":
        print(endpoint["socket"])
        return
    client = Client(endpoint)
    try:
        if sys.argv[1] in ("--list", "-l"):
            print("\n".join(sorted(client.tools())))
            return
        args = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
        print(client.call(sys.argv[1], args))
    finally:
        client.close()


if __name__ == "__main__":
    main()
