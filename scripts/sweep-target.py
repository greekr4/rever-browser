#!/usr/bin/env python3
"""Delete superseded build output under target/debug.

Cargo keeps one directory per build variant (target/debug/build/<package>/<hash>) and
never removes one when another replaces it, so they pile up: the app's 250MB binary
alone was there 54 times, most of Servo a dozen times.

Which variants are still used cannot be read from timestamps (the Servo build in use
was older than three dead ones), so cargo is asked: COMMANDS are run with JSON output,
which names every file they use. They build first if something is out of date; when a
build fails nothing is deleted. Everything else older than MIN_AGE_DAYS goes.

A variant another command uses (a single crate's preview, port-harness) is rebuilt the
next time that command runs.

    scripts/sweep-target.py            delete
    scripts/sweep-target.py --dry-run  only report

The cargo runner (scripts/cargo-runner.sh) starts this once a day in the background.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

# The commands of CLAUDE.md and CI: the variants they use are the ones to keep.
COMMANDS = [["build", "-p", "rever"], ["test", "--workspace", "--no-run"]]
# Younger output stays whatever cargo says: a build may be writing it right now.
MIN_AGE_DAYS = 1
# Incremental caches per crate that stay; cargo does not say which ones it uses.
KEEP_INCREMENTAL = 4

VARIANT = re.compile(r"/build/([^/]+)/([0-9a-f]{16})/")


def variants_in(messages) -> set:
    """The (package, hash) directories the files of cargo's JSON messages are in."""
    live = set()
    for message in messages:
        paths = list(message.get("filenames") or [])
        paths += [message.get("out_dir") or "", message.get("executable") or ""]
        for path in paths:
            found = VARIANT.search(path + "/")
            if found:
                live.add(found.groups())
    return live


def live_variants(root: Path):
    """What COMMANDS use, or None when one of them does not build."""
    live = set()
    for command in COMMANDS:
        run = subprocess.run(
            ["cargo", *command, "--message-format=json"],
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        messages = [json.loads(line) for line in run.stdout.splitlines() if line.startswith("{")]
        if not any(m.get("reason") == "build-finished" and m.get("success") for m in messages):
            print(f"cargo {' '.join(command)} did not build; nothing deleted")
            return None
        live |= variants_in(messages)
    return live


def newest_mtime(path: Path) -> float:
    return max((f.stat().st_mtime for f in path.glob("*")), default=0.0)


def beyond_newest(groups: dict, keep: int) -> list:
    """From {group: [(mtime, path)]}: all but the newest `keep` of each group."""
    return [
        path
        for variants in groups.values()
        for _, path in sorted(variants, reverse=True)[keep:]
    ]


def dead_builds(debug: Path, live: set) -> list:
    dead, uplifted = [], defaultdict(list)
    for package in (debug / "build").glob("*"):
        for variant in package.glob("*"):
            fingerprint = variant / "fingerprint"
            if (package.name, variant.name) in live or not fingerprint.is_dir():
                continue
            kind = next((f.stem for f in fingerprint.glob("*.json")), "")
            # Binaries and examples are copied to target/debug, so cargo names the copy
            # and not the variant: the last one built is the one in use.
            if kind.startswith(("bin-", "example-")):
                uplifted[(package.name, kind)].append((newest_mtime(fingerprint), variant))
            else:
                dead.append(variant)
    return dead + beyond_newest(uplifted, 1)


def dead_incremental(debug: Path) -> list:
    groups = defaultdict(list)
    for variant in (debug / "incremental").glob("*-*"):
        groups[variant.name.rsplit("-", 1)[0]].append((variant.stat().st_mtime, variant))
    return beyond_newest(groups, KEEP_INCREMENTAL)


def disk_size(paths: list) -> int:
    """Bytes on disk, every hard-linked file counted once."""
    blocks = {}
    for path in paths:
        for directory, _, files in os.walk(path):
            for name in files:
                stat = os.lstat(os.path.join(directory, name))
                blocks[stat.st_ino] = stat.st_blocks
    return sum(blocks.values()) * 512


def main() -> None:
    dry_run = "--dry-run" in sys.argv
    root = Path(__file__).resolve().parent.parent
    debug = root / "target" / "debug"
    live = live_variants(root)
    if live is None:
        sys.exit(1)
    cutoff = time.time() - MIN_AGE_DAYS * 86400
    dead = [
        path
        for path in dead_builds(debug, live) + dead_incremental(debug)
        if newest_mtime(path) < cutoff and path.stat().st_mtime < cutoff
    ]
    size = disk_size(dead)
    if not dry_run:
        for path in dead:
            shutil.rmtree(path, ignore_errors=True)
    verb = "would delete" if dry_run else "deleted"
    print(f"{verb} {len(dead)} directories, about {size / 2**30:.1f} GB ({len(live)} in use)")


def self_check() -> None:
    messages = [
        {"filenames": ["/r/target/debug/build/tokio/9fec0292366bae03/out/libtokio.rlib"]},
        {"out_dir": "/r/target/debug/build/stylo/2398af194549d66f/out"},
        {"executable": "/r/target/debug/build/rever/233e0982e6197323/out/rever-233e"},
        {"filenames": ["/r/target/debug/rever"], "executable": None},
    ]
    assert variants_in(messages) == {
        ("tokio", "9fec0292366bae03"),
        ("stylo", "2398af194549d66f"),
        ("rever", "233e0982e6197323"),
    }
    groups = {"a": [(1, "old"), (3, "new"), (2, "mid")], "b": [(1, "only")]}
    assert beyond_newest(groups, 1) == ["mid", "old"]
    assert beyond_newest(groups, 4) == []


if __name__ == "__main__":
    self_check()
    main()
