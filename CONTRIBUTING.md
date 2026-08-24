# Contributing to rever-browser

Thanks for taking the time to contribute.

## Getting set up

```bash
bun install      # install dependencies (use bun, not npm/pnpm)
bun run dev      # electron-vite dev — main + preload + renderer with HMR
```

If HMR doesn't pick up a change to main- or preload-process code, kill the Electron process and re-run `bun run dev`:

```bash
pgrep -f "Electron|electron-vite" | xargs -r kill -9
```

See `CLAUDE.md` for the architecture overview (process boundaries, data flows, MCP server layout) before touching `src/main/` or `src/preload/`.

## Before opening a PR

```bash
bun run typecheck  # tsc on tsconfig.node.json + tsconfig.web.json
bun run test        # vitest (unit tests)
```

Both must pass. If your change touches a browser tool (MCP tools, CDP, snapshot/click/type), see `docs/agent-testing.md` for how to exercise it against `test-fixtures/` — don't rely on typecheck/unit tests alone for that surface.

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
