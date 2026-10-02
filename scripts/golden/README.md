# Golden fixtures

Parity tests between the desktop's TypeScript and the Rust core (plan §6.1).
Until P6 removes the TypeScript copies, a ported function must return exactly
what the desktop returns.

- `scripts/golden/*.ts` call the real desktop functions (imported from
  `electron/`, `src/lib/`) on fixed inputs.
- `shared/golden/<set>.jsonl` stores the results, one case per line.
- `crates/core/tests/golden.rs` runs the Rust port on the same inputs and
  compares its JSON with the stored output byte for byte.

| Set | Desktop function | Rust port |
|---|---|---|
| `extract-content-terms` | `electron/db.ts#extractContentTerms` | `shelfy_core::search::terms::extract_content_terms` |
| `merge/*` (one file per scenario) | `electron/db.ts#bulkUpsert` | `shelfy_core::ingest::merge::upsert_batch` |

### Stateful sets: `merge/`

A function that writes to the desktop library is checked by scenarios. Each
file in `shared/golden/merge/` is one scenario, and its cases are steps that
run in order on one fresh library: the args hold the step's batch, the output
holds what the function returned and a view of the whole library afterwards.
`scripts/golden/merge.ts` documents the view; `crates/core/tests/golden_merge/`
maps the steps onto the web schema and reads the same view back.

## File format

JSON Lines. The first line is a header, every other line is a case:

```
{"golden":"extract-content-terms","source":"electron/db.ts#extractContentTerms","generator":"scripts/golden/extract-content-terms.ts","format":1}
{"id":"italian","args":["lampada da tavolo in vetro",{}],"output":["lampada","tavolo","vetro"]}
```

`output` is exactly what `JSON.stringify` wrote. The Rust test reads it as raw
text and compares it with `serde_json::to_string` of its own result, so key
order, number formatting and string escapes must match too. The `.jsonl`
extension also keeps Prettier from reformatting the files.

## Regenerate

From the repo root:

```sh
pnpm exec tsx scripts/golden/run.ts              # rewrite every set
pnpm exec tsx scripts/golden/run.ts <set>...     # rewrite some sets (`merge` = every merge/ file)
pnpm exec tsx scripts/golden/run.ts --check      # exit 1 if a file is stale or orphaned
```

Regenerate after changing a desktop function or a generator's inputs, then run
`cargo test -p shelfy-core --test golden`. A failure lists every case where the
Rust port now differs: fix the port (or, if the desktop change was a bug,
the desktop), never the fixture by hand.

`--check` needs no Rust toolchain; CI can run it to catch a desktop change that
was not followed by a fixture update.

## Add a function

1. Write `scripts/golden/<set>.ts` exporting a `GoldenSet` (see `lib.ts` and
   `extract-content-terms.ts`): the cases call the desktop function directly.
   Cover edge cases on purpose: empty input, Unicode, limits.
2. Register it in `SETS` in `run.ts` and generate the file.
3. Add a test to `crates/core/tests/golden.rs` that calls `check(...)` with the
   Rust port, and add the set name to `CHECKED` (`<dir>/` covers a directory
   of sets). A `.jsonl` file without a check fails
   `every_golden_file_has_a_check`.
4. Add a row to the table above.

Generators run under plain Node through `tsx`. Importing `electron/db.ts`
works because its native and Electron dependencies load lazily; a desktop
module that touches Electron at import time needs an `electron` shim like the
one in `scripts/search-eval/run.ts`.

A generator that needs the desktop database calls `openDesktopDb()` from
`lib.ts`: a private copy of `electron/db.ts` on a fresh in-memory library,
with the real schema and migrations. Wrap the calls in `withDesktopClock()` so
`Date.now()` and SQLite's `now` are pinned and the output does not depend on
when it ran. better-sqlite3 loads under plain Node because `pnpm install`
builds it for Node's ABI (CI's `test` job too); after a rebuild for Electron,
run `ELECTRON_RUN_AS_NODE=1 NODE_OPTIONS=--import=tsx electron scripts/golden/run.ts`.
