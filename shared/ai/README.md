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
| `score.ts` | per-field scores of catalog answers against a gold file |
| `fixtures/` | synthetic posts, answers and gold for offline tests (`posts.json`, `answers.json`, `invalid-answers.json`, `gold.json`) |

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
| `web_design` | rich website design catalog (P3-27), matching `electron/webcap/ai-catalog.ts` | `web_design.*` | `frames`, `digest`, `ground` |
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
one appears in these files. Rust reads the manifest strictly: a new field needs
its place in `TaskSpec` of both `prompts.ts` and `crates/core/src/ai/prompts.rs`,
and a new file its line in that module's `FILES` (tests name what is missing).

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

## Scoring a run against a gold file

```sh
pnpm exec tsx scripts/ai-eval/score.ts --gold=<gold.json> --answers=<answers.json> [--json=<report.json>]
```

- **gold.json**: `{"posts": {"<id>": {"kind": "social" | "web", "mediaType"?, "catalog": {…}}}}`,
  where `catalog` is the answer a perfect model would give, in the task's schema.
  A tag may be a list of acceptable alternatives: `["walnut", ["walnut wood", "walnut veneer"]]`.
- **answers.json**: `{"<id>": <the model's answer: the JSON object, its raw text, or null>}`.

Each answer is checked against its schema (off the schema scores 0, as the web
server would store nothing), normalized like the products do, then scored per
field: tag lists by F1 of matched terms (plurals, case, accents, trademark marks,
`#`, `-` and spacing ignored; no partial matches, so list alternatives in the
gold), entities by F1, keywords by a soft token F1, description and save reason
by content-word F1, language, purpose and industry exactly. Explicit `@handles`
retain dot/underscore identity. Scorer v2 also reports entity micro precision,
recall and F1, including missing answers, and false positives on empty entity
gold. Language has weight 0.02 because the current owner gold is uniformly
English. Recompute both sides with the same scorer version before comparing.
The report gives the mean per field, a weighted composite (`WEIGHTS` in
`score.ts`), the composite per media type, the worst posts, and the digest of
the catalog prompts currently loaded by the scoring command. For historical
answers, use the run manifest's digest as their provenance.
`fixtures/gold.json` shows the format. Real gold and answers are owner data:
keep them outside the repo (`../shelfy-web-local/bench40/` for X1), never commit
captions, gold, answers, media, provider URLs or credentials.

The serial operator-node runner is `scripts/ai-eval/run.ts`; its private env
provides `SHELFY_EVAL_ORNITH_BASE_URL`, `SHELFY_EVAL_ORNITH_API_KEY` and
`SHELFY_EVAL_ORNITH_VISION_MODEL`. Invoke it with Node 24, `--import=tsx`,
`--input=<private-bench40>`, `--out=<fresh-private-run-dir>`,
`--pipeline=baseline|candidate`, candidate `--media=deep|poster` (default poster),
and optionally `--limit=8`. It checks and decodes declared
media before contacting the node, refuses output paths inside either the
worktree or primary checkout even through symlink ancestors, and writes 0700
directories / 0600 JSON files. Baseline sends up to four extracted video frames
at 448 px and up to eight carousel images. Candidate mirrors P3-13 deep media
ordering: cover first, at most eight slides, video poster followed by up to
four equal-span midpoint keyframes, six JPEG images total at 1024 px; cover
and image slides are deduplicated by digest. It trims trailing hashtag walls
with the engine's rule. The independent private extraction is not the engine's
production CAS, so this benchmark cannot validate production video-object
availability. Poster mode uses only existing cover/image-slide/exported-poster
stills, never decodes video or substitutes extracted keyframes. It represents
a production library without video objects, retaining the candidate's 1024 px
still size. Neither pipeline sends transcripts.

Candidate `--hashtags=weak` is the engine-aligned default: it retains the
original caption, still bounded and marker-stripped by the shared builder,
leaving hashtag interpretation to the prompt. The manifest records
`captionPolicy`. `--hashtags=trim` retains the earlier trailing-wall rule as an
explicit experimental comparison. Enqueue the production engine with
`deep=true` to match the benchmark's 1024 px stills; `deep=false` uses 480 px.
When production video objects are absent, `deep=true` sends only available
stills and does not download videos.

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

## Website design parity

The legacy `web_catalog` manifest task returns purpose, industry and tag tiers.
The desktop capture pipeline uses a different design v2 contract: observations,
secondary site type, audience, style, theme, colour mood, density, layout, hero,
imagery, typography, components, craft, notable details, reference uses, summary
and description. New server captures use the additive `web_design` namespace,
whose system prompt and schema are pinned against the actual desktop exports.
The original social and legacy website contracts remain versioned independently.

`ai::web_design::map` matches the actual desktop `mapCatalog`: every output field,
all 19 facet families, font-class corrections, technology confidence threshold,
legacy tag tiers/entities/keywords, language and save reason. The complete result
is stored in `posts.ai_web_json`; existing website facets and similarity read it.
Input metadata accepts both migrated v1 captures and P4's nested
`meta_json.metadata` shape, including palette, fonts, technologies, motion/layout
traits and awards. The sanitized page digest has an 8,000 UTF-16-unit budget.

The server's image budget follows P3-27: hero plus up to three stored bands or
sections at 768 px. The desktop also composes an overview and varies screenshot
budgets by provider; those media-selection rules are not claimed as identical.
No screenshot, font, colour or motion evidence is inferred from remote downloads.

Parity fixtures live under `shared/golden/ai/web-design/`; regenerate with
`pnpm exec tsx scripts/golden/run.ts shared-ai-index ai/web-design`. Server tests
exercise the scrubbed P4 recorded capture, four actual JPEG inputs, measured
facets and the capture-id fence.
