export const meta = {
  name: 'port-units-to-rust',
  description: 'Port clusters of TypeScript modules to Rust crates (one crate per unit), then independently review each port against the TypeScript',
  phases: [
    { title: 'Port', detail: 'one agent per crate ports the modules and their tests' },
    { title: 'Review', detail: 'one independent agent per crate checks parity against the TypeScript and fixes gaps' },
  ],
}

const ROOT = '/Users/tk/Desktop/project/rever-browser'
// args.units: [{ crate, reviewOnly?, resume? }]. The files and notes of each unit live in
// scripts/workflows/units/<crate>.json so a run needs only crate names.
const UNITS = args.units
const UNIT_FILE = (unit) =>
  `Read ${ROOT}/scripts/workflows/units/${unit.crate}.json first: "files" are the TypeScript files of this unit (paths from the repo root), "notes" are the instructions specific to this crate. Follow the notes.`

const COMMON = `
You are porting part of an Electron/TypeScript app to Rust. Repository: ${ROOT} (a Cargo workspace; the TypeScript lives under src/).
The Rust port must behave like the TypeScript: same limits and constants, same edge-case behavior, same JSON field names (camelCase via serde) wherever a value is serialized. Prefer a faithful port over a redesign.

Rules for this workspace:
- Work ONLY inside your crate's directory. Do not edit the workspace Cargo.toml, other crates, or anything under src/. You may add dependencies to your own crate's Cargo.toml when the port needs them (for example regex, base64, url); keep them few. Other crates of this workspace that are already ported (rever-core, rever-scope, rever-formats, rever-agent) may be used as path dependencies, read-only.
- One Rust module per TypeScript file. Each Rust file starts with a doc comment whose first line contains exactly: Port of \`<path of the TypeScript file from the repo root>\` — a scoreboard counts ported files by this marker, so the path must be exact and in backticks. Use the marker only for a file you ported completely.
- If a TypeScript file has a sibling test file (same name with .test.ts), port EVERY test case: each \`it(...)\`/\`test(...)\` becomes a Rust #[test] in the same module, checking the same expectation with the same inputs. If it has no test file, write tests yourself for the behavior that matters (branches, limits, malformed input). Test function names are written in Korean (Rust allows non-ASCII identifiers), describing the behavior, for example \`fn 같은_id로_다시_넣으면_필드를_병합한다()\`. Never weaken or drop a case to make it pass; if a case truly cannot be ported, leave it out and list it with the reason in your report.
- Module-level mutable state in the TypeScript (maps, arrays, counters) becomes a struct with methods, not a global, unless the notes for your crate say otherwise.
- No panics on untrusted input: no unwrap/expect/indexing that can fail on data coming from outside; return Option/Result instead. JavaScript and Rust differ in ways tests often miss: string length (UTF-16 units vs bytes), \\s and \\b in regex, the regex crate has no lookaround or backreferences, sort stability, number formatting, truthiness. Match the TypeScript behavior.
- Comments say why, not what, and are in English. Keep the non-obvious "why" comments from the TypeScript.
- Verify with: cargo test -p <your crate>   (run from ${ROOT}). Also run: cargo fmt -p <your crate>. Do not build or run other packages, do not run git commit/checkout/stash, and do not start the app.
- Other agents are working in other crates of the same workspace at the same time. If cargo reports it is waiting for a lock, wait. If the build fails because of a file outside your crate, do not fix it: retry a little later, and report it if it persists.
`

const PORT_SCHEMA = {
  type: 'object',
  properties: {
    crate: { type: 'string' },
    modules: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          tsFile: { type: 'string' },
          rustFile: { type: 'string' },
          tsTestCases: { type: 'integer', description: 'number of it()/test() cases in the TypeScript test file, 0 if there is none' },
          rustTests: { type: 'integer', description: 'number of #[test] functions for this module' },
        },
        required: ['tsFile', 'rustFile', 'tsTestCases', 'rustTests'],
      },
    },
    testsPass: { type: 'boolean', description: 'cargo test -p <crate> exits 0' },
    testCount: { type: 'integer' },
    notPorted: {
      type: 'array',
      items: {
        type: 'object',
        properties: { what: { type: 'string' }, reason: { type: 'string' } },
        required: ['what', 'reason'],
      },
      description: 'functions or test cases left out, with the reason',
    },
    behaviorDifferences: { type: 'array', items: { type: 'string' }, description: 'known places where the Rust behaves differently from the TypeScript' },
    dependenciesAdded: { type: 'array', items: { type: 'string' } },
  },
  required: ['crate', 'modules', 'testsPass', 'testCount', 'notPorted', 'behaviorDifferences', 'dependenciesAdded'],
}

const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    crate: { type: 'string' },
    testsPass: { type: 'boolean', description: 'cargo test -p <crate> exits 0 after your changes' },
    testCount: { type: 'integer' },
    missingCasesAdded: { type: 'array', items: { type: 'string' } },
    bugsFixed: {
      type: 'array',
      items: {
        type: 'object',
        properties: { file: { type: 'string' }, what: { type: 'string' } },
        required: ['file', 'what'],
      },
      description: 'places where the Rust behaved differently from the TypeScript and you fixed it',
    },
    remaining: {
      type: 'array',
      items: {
        type: 'object',
        properties: { file: { type: 'string' }, what: { type: 'string' } },
        required: ['file', 'what'],
      },
      description: 'differences or gaps you found but did not fix, with the reason',
    },
    markersCorrect: { type: 'boolean', description: 'every fully ported Rust module starts with the exact "Port of `<ts path>`" marker, and no partly ported file carries one' },
  },
  required: ['crate', 'testsPass', 'testCount', 'missingCasesAdded', 'bugsFixed', 'remaining', 'markersCorrect'],
}

const results = await pipeline(
  UNITS,
  (unit) =>
    // reviewOnly: the port finished in an earlier run; only the review is owed.
    unit.reviewOnly ? Promise.resolve({ notPorted: [] }) :
    agent(
      `${COMMON}
Your crate: ${unit.crate} (directory ${ROOT}/${unit.crate}).
${UNIT_FILE(unit)}
Port the TypeScript files it lists (and their .test.ts siblings where they exist) into your crate.
${unit.resume ? `\nA previous agent was interrupted partway through this crate, so it already holds partial work. Read what is there first, keep what is correct, and finish the rest; do not start over.\n` : ''}
Read each TypeScript file and its test file fully before writing. When all modules are ported and cargo test -p ${unit.crate} passes, report.`,
      { label: `port:${unit.crate}`, phase: 'Port', schema: PORT_SCHEMA }
    ),
  (port, unit) =>
    agent(
      `${COMMON}
You are the independent reviewer of a port that another agent just finished. Crate: ${unit.crate} (directory ${ROOT}/${unit.crate}).
It is meant to be a faithful Rust port of the TypeScript files listed in its unit file (and their .test.ts siblings where they exist).
${UNIT_FILE(unit)}

Do not trust that it is correct or complete. Check it against the TypeScript yourself:
1. For every TypeScript test file, list its it()/test() cases and find the Rust #[test] that checks the same expectation with the same inputs. Add any that are missing or that were weakened (looser assertion, fewer inputs). For files without a TypeScript test, check that the Rust tests cover the branches and limits that matter and add what is missing.
2. Read each TypeScript function next to its Rust counterpart and look for behavior differences the tests do not cover: off-by-one limits, different constants, string length measured in bytes where the TypeScript counts UTF-16 units or characters, case sensitivity, regex differences between JavaScript and the Rust regex crate, sort stability and ordering, missing branches, different handling of empty or malformed input. Where it is cheap, run the original TypeScript with bun on tricky inputs and compare. Write a test that shows each real difference, then fix the code.
3. Check that exported TypeScript functions all have a Rust counterpart, that no untrusted input can panic, and that each fully ported Rust module starts with the exact "Port of \`<ts path>\`" marker.
4. Run cargo test -p ${unit.crate} and cargo fmt -p ${unit.crate}. Delete any temporary scripts you created.

Fix what you find inside the crate; report only what is real. If something cannot be fixed inside this crate, list it under remaining with the reason.${port && port.notPorted && port.notPorted.length ? `\n\nThe porting agent reported leaving these out — check whether each reason holds:\n${port.notPorted.map((n) => `- ${n.what}: ${n.reason}`).join('\n')}` : ''}`,
      { label: `review:${unit.crate}`, phase: 'Review', schema: REVIEW_SCHEMA }
    ).then((review) => ({ crate: unit.crate, port, review }))
)

return results
