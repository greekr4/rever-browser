#!/bin/sh
# Cargo's runner for this workspace (.cargo/config.toml): `cargo run` and `cargo test`
# start their binaries through it. Once a day it starts the sweep of superseded build
# output in the background, then runs the binary as cargo would have.
root="$(cd "$(dirname "$0")/.." && pwd)"
stamp="$root/target/.sweep-stamp"
if [ -z "$CI" ] && [ -z "$(find "$stamp" -mtime -1 2>/dev/null)" ]; then
  touch "$stamp"
  # A clean environment: the variables cargo sets for the binary must not reach the
  # cargo the sweep runs.
  (cd "$root" && env -i HOME="$HOME" PATH="$PATH" nohup nice python3 scripts/sweep-target.py \
    >target/sweep.log 2>&1 &)
fi
exec "$@"
