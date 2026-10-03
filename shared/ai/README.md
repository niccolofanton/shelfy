# shared/ai — the AI prompts and schemas

Every prompt and response schema Shelfy sends to a model (plan §2.15), shared by
the desktop app and the web server so that both products catalog the same way.

| Reader | How it reads them |
|---|---|
| Desktop, `electron/analyzer.ts` | `catalog.ts` and `prompts.ts`, over the generated `index.ts`; `build/esbuild-electron.ts` inlines them into the analyzer's output |
| Web server, `crates/core/src/ai/` | the files themselves, through `include_str!` |
| Scripts and tests | the same TypeScript modules |

## Files

| File | What |
|---|---|
| `manifest.json` | `schemaVersion` and the tasks (below) |
| `<task>.system.md`, `<task>.user.md` | the prompts, as templates (syntax below) |
| `<task>.schema.json` | the JSON Schema of the answer, exactly as sent to providers |
| `index.ts` | **generated** from the files above; never edit it |
| `template.ts` | the template renderer |
| `prompts.ts` | the tasks: rendered prompts, response schemas, sampling |
| `catalog.ts` | the catalog prompts and the normalization of their answers |
| `fixtures/` | synthetic posts and answers for offline tests (`posts.json`, `answers.json`, `invalid-answers.json`) |

A prompt or schema file that the manifest does not name fails the generator.

## The Rust side

`crates/core/src/ai/` reads the same files with `include_str!` (the image
build copies `shared/ai/` for it):

| Module | What |
|---|---|
| `prompts` | the manifest's tasks: `system_prompt`, `user_prompt`, `response_schema` (compact JSON in the file's key order, for providers), `max_tokens`, `SCHEMA_VERSION` (2) |
| `template` | the template renderer |
| `catalog` | `CatalogKind::of(platform, media_type)` and `request(kind, text, hints, has_frames)` |
| `normalize` | `catalog(kind, answer)`: the answer checked against its schema (an `OutputError` otherwise, nothing written), then normalized; `Catalog::into_patch(provider, model)`: the `AiPatch` of a finished analysis (status `done`, `ai_schema_version` 2, tiers) |

Golden sets under `shared/golden/ai/catalog/` (written by
`scripts/golden/ai-catalog.ts`) pin the Rust port to the desktop byte for byte:
the renderer, the markers, the user prompts, whole requests, the normalization
and the layer an answer writes.

## Tasks

| Task | Desktop use | Files | Template variables |
|---|---|---|---|
| `catalog` | social cataloging (AI-08) | `catalog.*` | `frames`, `caption`, `vocabulary` |
| `web_catalog` | website cataloging (AI-09) | `web_catalog.*` | `frames`, `caption`, `tech`, `purposes`, `industries` |
| `qc` | screenshot quality check (AI-23) | `qc.*` | — |
| `chat` | the search chat (AI-35) | `chat.system.md` | `broad`, `specific`, `active`, `perTierCap`, `maxKeywords`, the sentinels `generalOpen` … `removeClose` |
| `suggest` | suggestion chips (AI-41) | `suggest.*` | `query` |
| `cluster_refine` | cluster refinement (AI-30) | `cluster_refine.*` | `tags` |
| `aliases` | alias proposals (AI-32) | `aliases.*`, `cluster_refine.system.md` | `candidates`, `vocabulary` |

A task's fields in `manifest.json`:

| Field | Meaning |
|---|---|
| `system`, `user` | its template files (`user` is absent for the chat, whose messages are the conversation) |
| `schema` | `{name, file}`: the schema's name for the provider and its file; absent for free text |
| `temperature` | sampling temperature |
| `maxTokens` | a number, or `{base, perItem, max}`: `min(max, base + perItem × items)` |
| `captionMax` | catalogs: the caption or page text is cut at this many UTF-16 units, then `…` |
| `hintsMax` | catalogs: at most this many vocabulary (social) or tech-stack (web) hints |
| `caps` | catalogs: the most `general`, `specific` and flat `tags` an answer keeps |

Only portable settings belong here. llama.cpp-only knobs (the DRY sampler,
`cache_prompt`, chat-template kwargs) stay in the desktop code; a test fails if
one appears in these files.

## Template syntax

A template is the file's text without its final line break, read line by line.

| Syntax | Meaning |
|---|---|
| `{{#if name}}` … `{{else}}` … `{{/if}}` | each on a line of its own: keeps the lines between when `name` is true or a non-empty text; sections nest, `{{else}}` is optional |
| `{{#unless name}}` … `{{/unless}}` | the same, inverted |
| `{{! comment }}` | a line of its own, never sent |
| `{{name}}` | anywhere in another line: the variable's text, inserted as it is |

Kept lines are joined with `\n`. A variable is never read again as template
syntax, so untrusted text cannot inject any. An unknown variable (in any
branch), an unbalanced section or a directive that is not alone on its line is
an error, so a typo fails the tests instead of reaching a model.
`template.ts` and `crates/core/src/ai/template.rs` implement these rules.

## Changing a prompt or a schema

1. Edit the files here (and the manifest for a new file or setting).
2. `pnpm exec tsx scripts/golden/run.ts` regenerates `index.ts` and the golden
   sets; `--check` fails while either is stale.
3. `cargo test -p shelfy-core` checks the Rust side against them.
4. `pnpm vitest run tests/electron/analyzer-requests.test.ts -u` records what
   the desktop now sends; review that diff.

An output-schema change bumps `schemaVersion` here and `SCHEMA_VERSION` in
`crates/core/src/ai/prompts.rs` (a test ties them); web results store it in
`ai_schema_version`. A catalog prompt change also needs a real-run report
before it lands (P3 lane rule 9).
