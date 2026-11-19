# Contributing to rever-browser

Thanks for taking the time to contribute.

## Getting set up

```bash
./scripts/apply-rust-patches.sh   # fetch + patch the vendored crates (first run)
cargo run -p rever -- <url>       # build and launch the app
```

See `CLAUDE.md` for the architecture overview (crates, engine tap, MCP server layout) before touching `rever/` or the `rever-tools-*` crates.

## Before opening a PR

```bash
cargo test --workspace   # every crate's unit tests
cargo build -p rever     # the app itself compiles
```

Both must pass. If your change touches a browser tool (MCP tools, the engine tap, snapshot/click/type), see `docs/agent-testing.md` for how to exercise it against `test-fixtures/` — don't rely on unit tests alone for that surface.

## Commit messages

`[TAG] Short description` (≤ ~50 chars), English only. Tags: `[FEAT]` `[MODIFY]` `[FIX]` `[REMOVE]` `[REFACTOR]` `[STYLE]` `[DOCS]` `[TEST]` `[CHORE]`.

```
[FIX] Guard webview attach before dom-ready
```

## Pull requests

- Keep PRs scoped to one change; unrelated cleanup makes review harder.
- Describe what changed and why, and how you tested it (commands run, or MCP tool calls for browser-facing changes).
- Link the issue it closes, if any.

## Reporting bugs / requesting features

Use the issue templates under **New Issue**. Include repro steps and environment (OS, app version) for bugs.

## Code style

Match the existing style in the file you're editing. Keep PRs surgical — don't refactor or reformat unrelated code in the same PR.

## License

By contributing, you agree your contributions are licensed under the project's [Apache-2.0](./LICENSE) license.
