#!/usr/bin/env python3
"""Compare how the app and Chrome draw the same pages, worst difference first.

For every URL it takes the top of the page in the running app (through
bin/revb.mjs) and in headless Chrome at the same viewport, scores how far the
two pictures are apart, and writes report.html with them side by side.

Python, not Rust: it only strings together revb (Node), the Chrome binary and
sips, and needs no build step. It uses nothing outside the standard library.

Usage:
    REVER_USER_DATA_DIR=<dir of a running instance> \\
        python3 scripts/render-compare.py urls.txt out-dir [--settle 10]

The score is the mean difference of the two pictures shrunk to 160 by 120 gray
pixels, in percent. Pages with ads or carousels never score zero: read it as an
order to look in, not as a measure.
"""
import argparse
import html
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
)
THUMB = (160, 120)


def app(tool: str, args: dict | None = None) -> str:
    """One revb call. A screenshot comes back as the path of the saved file."""
    import json

    out = subprocess.run(
        ["node", os.path.join(HERE, "..", "bin", "revb.mjs"), "call", tool, json.dumps(args or {})],
        capture_output=True, text=True, timeout=240,
    )
    if out.returncode != 0:
        raise SystemExit(f"revb {tool} failed: {out.stderr.strip()}")
    return out.stdout.strip()


def app_shot(url: str, settle: float, path: str) -> bool:
    app("scope_add", {"hosts": [urllib.parse.urlsplit(url).hostname]})
    app("browser_navigate", {"url": url})
    time.sleep(settle)
    saved = app("browser_screenshot")
    if not saved or not os.path.exists(saved):
        return False
    shutil.copy(saved, path)
    return True


def chrome_shot(url: str, size: tuple[int, int], settle: float, path: str) -> bool:
    # Chrome can still be writing its profile when the folder is removed.
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as profile:
        command = [
                CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars",
                "--no-first-run", "--lang=ko-KR", "--force-device-scale-factor=1",
                # The light scheme, which the app draws: a dark system would otherwise
                # put every page that follows it at the top of the list.
                "--blink-settings=preferredColorScheme=1",
                f"--window-size={size[0]},{size[1]}", f"--user-agent={USER_AGENT}",
                f"--user-data-dir={profile}", f"--virtual-time-budget={int(settle * 1000)}",
                f"--screenshot={path}", url,
        ]
        try:
            subprocess.run(command, capture_output=True, timeout=60)
        except subprocess.TimeoutExpired:
            # Some pages never let headless Chrome finish; the row is reported without a picture.
            pass
    return os.path.exists(path)


def gray(path: str) -> list[int] | None:
    """The picture as THUMB gray pixels, read from the BMP sips writes."""
    with tempfile.NamedTemporaryFile(suffix=".bmp") as small:
        done = subprocess.run(
            ["sips", "-s", "format", "bmp", "-z", str(THUMB[1]), str(THUMB[0]),
             path, "--out", small.name],
            capture_output=True,
        )
        if done.returncode != 0:
            return None
        data = open(small.name, "rb").read()
    offset, = struct.unpack_from("<I", data, 10)
    width, height, _, bits = struct.unpack_from("<iiHH", data, 18)
    step = bits // 8
    row = (width * step + 3) & ~3
    pixels = []
    for y in range(abs(height)):
        # A positive height stores the rows bottom-up.
        line = offset + row * (abs(height) - 1 - y if height > 0 else y)
        for x in range(width):
            b, g, r = data[line + x * step:line + x * step + 3]
            pixels.append((r * 299 + g * 587 + b * 114) // 1000)
    return pixels


def score(a: str, b: str) -> float | None:
    one, two = gray(a), gray(b)
    if not one or not two or len(one) != len(two):
        return None
    return sum(abs(p - q) for p, q in zip(one, two)) * 100 / (255 * len(one))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("urls")
    parser.add_argument("out")
    parser.add_argument("--settle", type=float, default=10)
    options = parser.parse_args()
    os.makedirs(options.out, exist_ok=True)
    urls = [line.strip() for line in open(options.urls) if line.strip() and line[0] != "#"]

    app("scope_add", {"hosts": ["127.0.0.1"]})
    size = re.search(r"(\d+)x(\d+)", app(
        "browser_evaluate", {"expression": "innerWidth + 'x' + innerHeight"}))
    viewport = (int(size.group(1)), int(size.group(2))) if size else (1020, 773)
    print(f"viewport {viewport[0]}x{viewport[1]}, {len(urls)} pages", flush=True)

    rows = []
    for index, url in enumerate(urls, 1):
        stem = f"{index:02d}-{urllib.parse.urlsplit(url).hostname}"
        ours, theirs = (os.path.join(options.out, f"{stem}-{who}.png") for who in ("rever", "chrome"))
        # A page both pictures exist for is not taken again: an interrupted run resumes.
        got_ours = os.path.exists(ours) or app_shot(url, options.settle, ours)
        got_theirs = os.path.exists(theirs) or chrome_shot(url, viewport, options.settle, theirs)
        value = score(ours, theirs) if got_ours and got_theirs else None
        rows.append((value, url, stem, got_ours, got_theirs))
        shown = "no picture" if value is None else f"{value:5.1f}%"
        missing = "".join(f" (no {who})" for who, got in (("app", got_ours), ("chrome", got_theirs)) if not got)
        print(f"{index:2d}/{len(urls)} {shown}  {url}{missing}", flush=True)

    rows.sort(key=lambda row: -1 if row[0] is None else row[0], reverse=True)
    cells = "".join(
        f"<tr><td>{'-' if value is None else f'{value:.1f}%'}<br>"
        f"<a href='{html.escape(url)}'>{html.escape(url)}</a></td>"
        f"<td><img src='{stem}-rever.png'></td><td><img src='{stem}-chrome.png'></td></tr>"
        for value, url, stem, _, _ in rows
    )
    with open(os.path.join(options.out, "report.html"), "w") as report:
        report.write(
            "<!doctype html><meta charset='utf-8'><title>Render compare</title>"
            "<style>body{font:13px system-ui}img{width:480px;border:1px solid #ccc}"
            "td{vertical-align:top;padding:6px}</style>"
            f"<table><tr><th>Difference</th><th>App</th><th>Chrome</th></tr>{cells}</table>"
        )
    print("report:", os.path.join(options.out, "report.html"))


if __name__ == "__main__":
    main()
