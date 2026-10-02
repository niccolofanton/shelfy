# SPIKE-5 — FTS5 relevance vs the desktop ranking

**Question.** Does the FTS5 search of D10 / §2.14 rank as well as the desktop's `LIKE` + JS relevance on the desktop's evaluation set (`scripts/search-eval`)?

**Pass criterion (§9, §2.14).** Over the six `search-eval` cases, mean nDCG@10 and mean MRR are at least the desktop's numbers in `last-report.json` minus 0.02.

**Answer.**
- **Gate: pass.** The gate compares against `last-report.json` on the library that report was measured on. There, FTS5 reaches **nDCG@10 0.653 and MRR 0.861**; the desktop has 0.620 and 0.861.
- **Like for like on the current reference library: MRR is equal, nDCG@10 is 0.019 short of the tolerance.** On the 2026-10-02 snapshot, the desktop harness scores **0.791 / 1.000** and FTS5 scores **0.752 / 1.000**.
  - Only one case, `fluidi`, causes the gap. Its cause is recall: three relevant posts contain a query term only inside a longer token. A token index cannot match there, while `LIKE '%term%'` can.
  - A trigram index closes the gap exactly, but it needs a schema change. It is left to P1-05 (see *Options*).
  - **Update (P1-05):** the trigram infix index is adopted (schema v2). Both libraries now pass, the 2026-10-02 one at desktop parity, and the gate also runs in CI on a synthetic library. See [P1-05](#p1-05-the-infix-index-and-the-gate-in-ci).
- **Tuning changed the score, not the index.**
  - Each term gets the desktop's clamped weight, `ln(N/df)` limited to 1–3, instead of bm25's unbounded IDF.
  - Whole-token hits count on top of prefix hits.
  - The tag boost scales with the tag's weight.
  - One-letter terms match whole tokens only.
- **Latency is well inside the budget.** The `GET /search` budget is p95 ≤ 60 ms (§6.2). Measured p95, first page plus count:
  - 3.3 ms on the 6,138-post library;
  - 20 ms on an 18,412-post library, over a query mix that includes stopword-only queries.

| Library | Desktop baseline | FTS5 before (T3) | FTS5 after (T4) | Gate |
|---|---|---|---|---|
| Baseline library (`last-report.json`, 2026-05-31) | 0.620 / 0.861 | 0.660 / 1.000 | **0.653 / 0.861** | **pass** |
| Reference snapshot (2026-10-02), desktop re-run on it | 0.791 / 1.000 | 0.700 / 0.917 | **0.752 / 1.000** | nDCG@10 short by 0.019 |

Values are mean nDCG@10 / mean MRR over the six cases. Measured on 2026-10-02 in a release build on the dev machine (Apple M1 Pro).

## Two libraries, two baselines

The reference snapshot is not the library `last-report.json` was measured on. Since that report, the library has grown, and its AI analysis was cleared.

| | Baseline library (2026-05-31) | Reference snapshot (2026-10-02) |
|---|---|---|
| Posts | 3,235 | 6,138 |
| Posts with AI fields | 134 | 4 |
| Gold-set sizes (cuffie, product, tipografia, shader, fluidi, p8-idf) | 9, 154, 103, 125, 27, 50 | 17, 285, 165, 165, 36, 81 |

Metrics from different libraries do not compare: the gold sets differ, and so does what the search can find. So:
- **Baseline library.** The desktop harness copies the library into `scripts/search-eval/.scratch/shelfy.sqlite`, and this copy is still there in the main checkout. Its gold sets match `last-report.json` exactly. Re-running the current desktop code on it reproduces every number of the report. The desktop ranking has not changed since.
- **Reference snapshot.** The desktop harness, unmodified, was run on a copy of the snapshot to get the like-for-like baseline.
- **Pairing guard.** `search_eval` refuses a pair whose gold-set sizes differ. The sizes come from the same SQL on the same rows.

## Method

**The port (`crates/core/tests/search_eval.rs` and `search_eval/`).**
- **Cases.** The six cases have the same ids, kinds, queries and terms as `cases.ts`. `cases_match_the_desktop_set` runs in CI and fails if the two lists drift apart.
- **Ground truth.** It uses the desktop oracle's own SQL. A post is relevant when its `text`, `ai_description`, `ai_keywords` or `ai_tags` is `LIKE '%gold term%'`. The query runs on a separate read-only connection to the desktop library, never through the code under test.
- **Search under test.** It is `repo::posts::list` with relevance order, the builder behind `GET /search` and `GET /posts?q=`. It takes the first page of 60, like the desktop harness, and runs inside `UserDb::read`. The total comes from `posts::count`.
- **Metrics.** From `order-metrics.ts`: p@5, p@10, r@5, r@10, MRR and nDCG@10, with the same denominators and null rules. From `run.ts`: `searchPrecision` and `searchRecall`. Also the hybrid probe: the top-2 gold tags plus the query.
  - Unit tests cover the metric functions.
  - Means are reported per case group (hard, mid, control) and overall.

**Not ported.**
- **Multi-run statistics (`--runs`).** The search is deterministic, so every run gives the same numbers.
- **The AI views, which arrive in P3.**
  - `poolRelevance`, `poolNoise`, `keywordRelevance` and the composite pass/fail score measure the AI tag retrieval of chat search.
  - The tag-only probe measures `searchPostsByTags`, whose Σ idf ranking has no core equivalent yet.
- **`humanGold`.** No case uses it. Its ids would be personal data, so a future human gold must come from a local file, not from the repo.

**The evaluation library.**
- Each run builds it in a temp dir, in 0.1–0.2 s:
  - `core::legacy` reads the rows;
  - `posts::insert` writes them with their AI layer, notes and manual tags, and indexes them;
  - accepted tag aliases are inserted first.
- Every desktop row becomes one post, so both apps rank the same documents.
- Media, collections and site captures are not copied. The desktop search reads none of them.

**Privacy.** The test prints counts and metrics only, and its optional JSON report holds the same. Nothing from the library is in the repo.

## Results per case group

Mean nDCG@10 / mean MRR. *Before* is T3's scoring (bm25, minus 3 per exact tag, minus 1.5 per phrase hit); *after* is this spike's.

| Library | Group | Desktop | Before | After |
|---|---|---|---|---|
| Baseline library | hard (2) | 0.692 / 1.000 | 0.692 / 1.000 | 0.692 / 1.000 |
| | mid (1) | 0.078 / 0.167 | 0.220 / 1.000 | 0.078 / 0.167 |
| | control (3) | 0.754 / 1.000 | 0.786 / 1.000 | 0.818 / 1.000 |
| | **all (6)** | **0.620 / 0.861** | **0.660 / 1.000** | **0.653 / 0.861** |
| Reference snapshot | hard (2) | 0.864 / 1.000 | 0.819 / 1.000 | 0.864 / 1.000 |
| | mid (1) | 0.220 / 1.000 | 0.220 / 1.000 | 0.220 / 1.000 |
| | control (3) | 0.934 / 1.000 | 0.781 / 0.833 | 0.855 / 1.000 |
| | **all (6)** | **0.791 / 1.000** | **0.700 / 0.917** | **0.752 / 1.000** |

How the tuned score changed each case:
- **`p8-idf`.** On the reference snapshot, bm25 ranked first a post that matches only "realizzati": the word is in one post, so its unbounded IDF beat "touchdesigner" (MRR 0.5). With the clamped weight, all top ten are relevant (1.000 / 1.000).
  - On the baseline library the case goes from 0.905 to 1.000; the desktop has 0.599.
- **`cuffie`.** On the reference snapshot it goes from 0.637 to 0.727, equal to the desktop.
  - "accessori" matches 13 posts, none relevant, and only as a prefix of longer words. "airpods" matches 6 relevant posts as a whole word.
  - The whole-token arm puts the "airpods" posts first, as the desktop's whole-word tier does.
- **`tipografia` on the baseline library.** It falls back to the desktop's numbers, 0.078 / 0.167.
  - bm25's unbounded IDF had put the one relevant post first, because "tipografia" was rarer than "animata".
  - Now both terms weigh the same. "animata" is a whole word in its 5 posts, none relevant, while "tipografia" only starts a longer word in the relevant post. The whole-token arm ranks the "animata" posts first, as the desktop's whole-word tier does.
  - Both rankings are weak here for the same reason: only 6 posts match the query at all. The relevant posts mostly use English words ("typography", "font", "kinetic") that the Italian query does not contain.

**Hybrid probe** (diagnostic, not gated), mean nDCG@10 over the probed cases:
- baseline library: FTS5 0.723, desktop 0.644 (5 cases);
- reference snapshot: equal (2 cases).

## Cases that still lose to the desktop

**`fluidi` (control).**

| Library | nDCG@10 FTS5 / desktop | Posts matched FTS5 / desktop |
|---|---|---|
| Baseline library | 0.454 / 0.662 | 4 / 7 |
| Reference snapshot | 0.564 / 0.801 | 4 / 7 |

- **This is recall, not ranking.** On the reference snapshot every post matched is relevant on both sides. On the baseline library both sides also match the same one non-relevant post (FTS5 p@10 0.750, the desktop 0.857).
- **The three posts only the desktop finds are relevant.** Their only match is a query term inside a longer token:
  - "fluidi" in the middle of a compound hashtag;
  - "solver" inside a longer word.
- **A token index cannot match there.**
  - `unicode61` indexes whole tokens, and an FTS5 prefix query matches the start of a token only.
  - `LIKE '%solver%'` matches anywhere.
- **The oracle favors the desktop on this point.** Its gold set is defined by the same `LIKE` substring rule as the desktop's retrieval. This is the circularity the `search-eval` README warns about.
  - Still, compound hashtags are common on Instagram, so the loss is real for users too.
- **The same mechanism cuts the totals elsewhere.** For example, `product` matches 530 posts instead of 726 on the baseline library. There it costs nothing at the top: nDCG@10 is 1.000 on both sides.

**Hybrid `tipografia`** (diagnostic only): 0.318 against the desktop's 0.428 on the baseline library.
- The desktop weighs a probe tag at 6 × its IDF. Here a tag is worth 3 × its weight, so text matches lead.
- With 6 × weight the case matches the desktop. But the hybrid mean then falls from 0.723 to 0.629, below the desktop's 0.644, so the factor stays 3.

## Final knobs and query rules

`crates/core/src/search/query.rs` holds the constants and the model; `repo::posts` applies them.

**Matching.** Unchanged from §2.14: `{tags keywords entities description note caption author web_text}: ("t1"* OR … OR "tn"*)` over the content terms of `extractContentTerms`. One exception: a term whose last token has a single character is matched as a whole token (`MIN_PREFIX_CHARS = 2`).
- Such a term is the whitelisted `r`, or a stopword-only query kept raw, such as `a`.
- As a prefix it matched every word with that initial.
- `posts_fts` has no 1-character prefix index (`prefix='2 3'`), so one query took 45 ms at 18k posts.

**Score** (lower is better, ties newest first):

```text
score = Σ_term w(term) × ( tf(prefix) + EXACT_TOKEN_WEIGHT × tf(exact) )
        − Σ_tag EXACT_TAG_BOOST × w(tag)      boost terms that are one of the post's tags
        − PHRASE_BONUS                         the whole query appears as a phrase
```

Where:
- `tf(x) = bm25(x) / idf(x)` for the single-phrase match `x`: bm25 with the column weights, divided by FTS5's own IDF.
- `w = clamp(ln(N / df), 1, 3)` is the desktop's `termIdfWeights`.
  - For a term, N is the number of indexed posts and df the number that prefix-match it.
  - For a tag, df is the number of posts carrying it, as in the desktop's `tagIdfWeights`.

| Knob | Value | Change |
|---|---|---|
| `BM25_WEIGHTS` (tags, keywords, entities, description, note, caption, author, web_text) | 6, 5, 4.5, 4, 4, 3.5, 2, 2 | kept |
| `TERM_WEIGHT_MIN` / `TERM_WEIGHT_MAX` | 1 / 3 | new: replaces bm25's IDF |
| `EXACT_TOKEN_WEIGHT` | 1.0 | new: whole-token hits count again |
| `EXACT_TAG_BOOST` | 3.0 × w(tag) | was a flat 3.0 |
| `PHRASE_BONUS` | 1.5 | kept |
| `MIN_PREFIX_CHARS` | 2 | new |
| `RELEVANCE_WINDOW` | 1,000 | kept |

**SQL.**
- **Statistics first.** `posts::list` counts N, the df of every match and the df of every boost tag. These are short `count(*)` queries on the caller's snapshot.
- **The ranked query** has two `MATERIALIZED` CTEs:
  - `arms`: one FTS5 scan per scored match, `bm25 × factor`;
  - `hits`: the sum per post.
- **No correlated FTS5 scan.** A unit test checks the plan: both CTEs materialized, one FTS5 scan per match, and no FTS5 scan under a correlated subquery.
- **Bug found and fixed.** With a single scored match, SQLite flattened the subquery into the aggregate and `bm25()` failed: "unable to use function bm25 in the requested context". The reference snapshot hit it with `tipografia`. The separate `arms` CTE fixes it, and a regression test covers it.

**Column weights could not be tuned.** These libraries have 4 and 134 posts with AI fields, so the cases rank almost only on captions. One property matters for later: FTS5's bm25 multiplies term frequency by the column weight before saturating it (k1 = 1.2).
- So one hit in a weight-6 column scores only about 12 % above one in a weight-3.5 column; the desktop's tiers differ by 71 %.
- Re-tune the weights once the library is AI-analyzed (P3). Smaller weights with the same ratios keep bm25 nearer its linear range.

## Options measured, not adopted

| Option | Baseline library | Reference snapshot | Cost | Decision |
|---|---|---|---|---|
| **Trigram infix index:** `fts5(tokenize='trigram remove_diacritics 1')` over the same text, used for matching. Infix-only hits score a constant × w. | 0.692 / 0.861 | **0.791 / 1.000** (desktop parity) | Needs a new table and migration. Index 6.2 MB beside `posts_fts`'s 3.6 MB on 6,138 posts. 0.1–0.4 ms per term. One more FTS write per post. | Recommended to P1-05. It is a schema change outside this lane. **Adopted in P1-05** (see below). |
| **Light stemming:** the prefix match uses the term minus a final vowel or plural `s` (terms of 5+ letters), so "fluidi" also finds "fluid", "fluido". | 0.728 / 0.854 | 0.846 / 0.917 | none | Not adopted. nDCG rises on both libraries, but MRR falls 0.08 on one or the other, depending on the variant. Six cases cannot settle it. |
| Exact and prefix as one bm25 query | 0.616 / 0.778 | 0.715 / 0.917 | none | rejected |
| Coverage first (distinct terms matched), then bm25 | 0.640 / 0.917 | 0.700 / 0.917 | none | rejected |
| Raw bm25 with the IDF capped at 3, 4 or 5 | 0.655 / 0.917 | 0.731 / 1.000 | none | rejected: no whole-token tier |

## Latency

Times are for the first page plus the count, release build, on the dev machine.

| Library | Queries | p50 | p95 | Max |
|---|---|---|---|---|
| Baseline library, 3,235 posts | the 6 cases × 20 | 0.5 ms | 2.5 ms | 2.7 ms |
| Reference snapshot, 6,138 posts | the 6 cases × 20 | 0.4 ms | 3.3 ms | 3.5 ms |
| Reference snapshot copied 3 times, 18,412 posts | the 6 cases + 10 generic, incl. stopword-only `a` and `the`, × 20 | 2.4 ms | 20 ms | 22 ms (`the`) |

At 18,412 posts:
- T3's scoring had a p95 of 36 ms (`a`: 37 ms).
- Before the one-letter rule, this spike's scoring had 46 ms.
- The slowest query left is `the`, a stopword kept raw that prefix-matches most English posts. The desktop matched it with `LIKE '%the%'`.
- VPS numbers belong to P1-05 / P1-26.

## How to re-run the gate

The gate needs **a desktop library and a `search-eval` report measured on those same rows**. The test checks this through the gold-set sizes. Without `SHELFY_SEARCH_EVAL_DB`, `cargo test` skips the gate on a real library; since P1-05 it always runs on the synthetic library (see [P1-05](#p1-05-the-infix-index-and-the-gate-in-ci)).

| Variable | Meaning |
|---|---|
| `SHELFY_SEARCH_EVAL_DB` | the desktop library (`shelfy.sqlite`), opened read-only |
| `SHELFY_SEARCH_EVAL_BASELINE` | the desktop report; default `scripts/search-eval/last-report.json` |
| `SHELFY_SEARCH_EVAL_REPORT` | optional: write a JSON report (aggregates only) |

**Frozen pairs.** Both pairs are kept outside the repo, in the lane data directory `../shelfy-web-local/data/t4/` (relative to the main checkout). They are the owner's data: read them only, and never commit them.

| Directory | Library | Desktop report | Expected result |
|---|---|---|---|
| `search-eval-2026-05-31/` | the baseline library: a copy of `scripts/search-eval/.scratch/shelfy.sqlite` | `last-report.json` | pass, 0.653 / 0.861 (T4); 0.692 / 0.861 since P1-05 |
| `search-eval-2026-10-02/` | the reference snapshot: a copy of `ref/shelfy.sqlite` | the desktop harness re-run on it | T4: nDCG@10 0.752 against 0.791, fails by 0.019; since P1-05 pass, 0.791 / 1.000 |

The copies matter: a `pnpm run eval:search` run overwrites `.scratch/shelfy.sqlite` and `last-report.json` in the main checkout with the live library.

**The §2.14 gate**, from the main checkout. The paths must be absolute, because cargo runs the test from `crates/core`:

```sh
PAIR="$PWD/../shelfy-web-local/data/t4/search-eval-2026-05-31"
SHELFY_SEARCH_EVAL_DB="$PAIR/shelfy.sqlite" SHELFY_SEARCH_EVAL_BASELINE="$PAIR/last-report.json" \
  cargo test --release -p shelfy-core --test search_eval -- --nocapture
```

**Any other library, for example the current one:**
1. Run the desktop harness on it, so the report and the rows match. `pnpm run eval:search` reads the live library and writes both `scripts/search-eval/.scratch/shelfy.sqlite` and `last-report.json`. A copy elsewhere needs `HOME` pointed at a directory that holds `Library/Application Support/Shelfy/shelfy.sqlite`, because the harness reads only that path.
   - `better-sqlite3` must be built for Electron. The main checkout's build is; a fresh worktree's is not.
2. Run the gate with `SHELFY_SEARCH_EVAL_DB="$PWD/scripts/search-eval/.scratch/shelfy.sqlite"` and the default baseline.

The test opens the library the way `core::legacy` does: immutable when there is no `-wal` file, so it leaves no side files.

**Output.**
- The run prints the per-case table, the group means, the hybrid probe and the latency.
- It fails when:
  - the pair does not match;
  - a mean is below the desktop's minus 0.02;
  - in a release build, p95 is over 60 ms.
- A debug build checks the metrics but not the latency.

## For T11 and P1-05

- **T11.** Call `posts::list` and `posts::count` inside one `UserDb::read`, so the statistics and the ranking see one snapshot.
  - A relevance page now runs these short counts before the ranked query: 1, plus up to 2 per term, plus 1 per boost term.
- **P1-05, the gate in CI.** CI can run the skip path, the drift check against `cases.ts` and the metric unit tests. It cannot run the gate itself: the gate needs the owner's library, and lane rule 9 keeps that library off CI.
  - Either the gate stays a manual lead/owner step on the frozen pair in `../shelfy-web-local/data/t4/`, or CI gets a synthetic library and a desktop report made from it.
- **P1-05, refreshing the baseline.** If `last-report.json` is refreshed from today's library, the gate fails by 0.019 nDCG@10 (`fluidi`) until infix matching lands. The frozen 2026-10-02 pair reproduces this. Decide on the trigram index, or keep the frozen 2026-05-31 pair as the gate's reference.
  - **Done in P1-05:** the infix index landed, and the 2026-10-02 pair passes at parity, so a refreshed baseline no longer fails the gate.
- **P3.** Re-run SPIKE-5 once posts carry AI fields, to tune `BM25_WEIGHTS` and the tag boost. Port the tag-only ranking (`searchPostsByTags`) with the AI views. The oracle reads the desktop library's AI fields, so an AI layer made only by the web app needs a new gold source.

## P1-05: the infix index and the gate in CI

P1-05 adopted the trigram infix index and moved the gate into CI. All numbers below are aggregates: means over the six cases, index sizes and latency percentiles. Measured on 2026-10-02 in release builds on the dev machine (Apple M1 Pro), while other lanes shared the machine.

**The index.** `posts_infix` is a second contentless FTS5 table (library schema v2, `0002_search_infix.sql`):
- `tokenize="trigram remove_diacritics 1"`, case-insensitive, `detail=full`. FTS5 refuses phrase queries without full position lists, and every term longer than 3 characters is a phrase of trigrams.
- One column per post: caption, AI description, note, author, the AI and manual tag, keyword and entity lists as stored, and the tag rows' forms. The text is one SQL expression, `search::index::INFIX_TEXT_SQL`, so the migration backfills the index in SQL (20,000 posts in about 1.3 s) and `rebuild_infix` re-derives it, for example after a rollback.
- `reindex_post`, `remove_post` and `rebuild` maintain it with `posts_fts`, in the same transaction; `index::verify` checks both.

**Matching and score.**
- A term of 3 or more characters matches anywhere in the text, as the desktop's `LIKE '%term%'` did: the search block is the token match or the infix match. Shorter terms (`3d`, `r`) stay token-only.
- Every post that holds the term anywhere adds `INFIX_WEIGHT` × w(term), with `INFIX_WEIGHT = 1.0`: the desktop's substring tier for a caption (1 × w), below any token hit.
- w(term) is the clamped weight of S5-2, from the token index's document frequency. A term found only inside longer tokens takes it from the infix index.

| Library | Desktop | FTS5 T4 | FTS5 P1-05 | Gate |
|---|---|---|---|---|
| Frozen 2026-05-31 pair (3,235 posts) | 0.620 / 0.861 | 0.653 / 0.861 | **0.692 / 0.861** | pass |
| Frozen 2026-10-02 pair (6,138 posts) | 0.791 / 1.000 | 0.752 / 1.000 | **0.791 / 1.000** | pass (was short by 0.019) |
| Synthetic library (3,000 posts, CI) | 0.905 / 1.000 | — | **0.940 / 1.000** | pass |

Values are mean nDCG@10 / mean MRR. Variants measured on both frozen pairs, all with the same means: `INFIX_WEIGHT` 0.5, 1, 2 and 3; scoring only the posts that the token index misses; taking w from the infix index's frequency. The simplest one was kept.

**Cost.**

| Library | `posts_fts` | `posts_infix` |
|---|---|---|
| 3,235 posts | 1.8 MB | 3.0 MB |
| 6,138 posts | 3.4 MB | 5.7 MB |
| 20,000 synthetic posts | 7.7 MB | 13.7 MB |

- About 0.7–0.9 KB per post, 1.7 times `posts_fts`; one more FTS5 write per post write.
- Latency of the gate's first page plus count, p95: 5.0 ms on the 2026-05-31 pair and 6.3 ms on the 2026-10-02 pair. T4 measured 2.5 and 3.3 ms without the infix arm and on a quieter machine.
- On the 20,000-post synthetic library, `admin bench` gives `GET /search` p95 11.3 ms against the 60 ms budget (P1-05 lane report).

**Paging and totals (§2.14).**
- `posts::rank` returns the first 1,000 post ids of a search, best first.
- The server caches that ranking per user, library state and filters. Cursors name the state, and every later page is cut from the same ranking, so a write between two pages neither repeats nor skips a post. A post trashed in between drops out of its page.
- When the ranking is shorter than 1,000, its length is the total, so most searches run no count.
- The gate measures `posts::list`, which is `rank` plus `ranked_page`, the builder the routes use.

**The gate in CI.** CI cannot read the owner's library, so it gets a synthetic one, paired with the desktop's own numbers on it.
- `search_eval/synthetic.rs` writes a desktop library (`shelfy.sqlite`, the desktop's current schema) from a fixed seed: 3,000 posts about the six cases' topics, near misses that share a query word without being relevant, and a background. It includes compound hashtags, Italian and English text, a little AI layer, and the reference platform mix.
- `search_eval/synthetic-report.json` holds the desktop harness's numbers on that file: aggregates only, all synthetic, with the library's fingerprint (`libraryDigest`).
- `search_eval_gate_on_the_synthetic_library` runs in every `cargo test`, so in the CI `rust` job. It refuses a report whose fingerprint or gold-set sizes differ. A debug build checks the metrics; a release build also checks the latency.
- The server test `search_ranking` builds the same library behind the HTTP routes. For every case, `GET /search` and `GET /posts?q=` return the core's ranking, page after page, with the core's total; so does the hybrid probe.
- The frozen pairs stay the check on real data, run locally (*How to re-run the gate*).

**Regenerating the synthetic report**, after a change to the generator or to `cases.ts`:
1. `SHELFY_SEARCH_EVAL_SYNTHETIC_OUT="$DIR/Library/Application Support/Shelfy/shelfy.sqlite" cargo test -p shelfy-core --test search_eval write_the_synthetic_library -- --ignored`
2. `HOME="$DIR" pnpm run eval:search`, which needs `better-sqlite3` built for Electron (see above). It writes `scripts/search-eval/last-report.json`, so keep a copy of the frozen one.
3. `cargo test -p shelfy-core --test search_eval record_the_synthetic_report -- --ignored`, which rewrites `synthetic-report.json` from that report after checking the gold-set sizes. Then restore the frozen `last-report.json`.

## Decisions

| # | Decision | Why |
|---|---|---|
| S5-1 | The gate compares a library only with a desktop report measured on the same rows; mismatched gold-set sizes fail the run. | Metrics from different libraries do not compare. |
| S5-2 | Term weight = the desktop's clamped `ln(N/df)`, applied to bm25's frequency part. | One rare filler word no longer outranks the topic. |
| S5-3 | Whole-token hits count on top of prefix hits. | This is the desktop's whole-word tier. |
| S5-4 | The tag boost scales with the tag's clamped weight. | It stays on the same scale as the term scores. |
| S5-5 | One-letter terms match whole tokens. | Latency (no 1-character prefix index) and meaning (`r`). |
| S5-6 | The index stays as specified (`unicode61`, prefix 2 and 3). Infix matching (trigram) goes to P1-05. | A schema change is outside this spike's scope. |
| S5-7 | No stemming for now. | Its effect is not stable across the two libraries. |
| P1-05-1 | Adopt the trigram infix index `posts_infix` (library schema v2): a term of 3 or more characters also matches anywhere in a post's text. | Desktop parity on both frozen pairs (the 2026-10-02 pair passes instead of failing by 0.019); terms inside compound hashtags are found again; about 0.7–0.9 KB of index per post; search stays well inside its budget. |
| P1-05-2 | Every post that holds a term anywhere adds `INFIX_WEIGHT` (1.0) × w(term); w comes from the token index, else from the infix index. | Every variant measured gave the same means; this one needs no extra subquery. |
| P1-05-3 | The gate runs in CI on a synthetic library paired with the desktop harness's numbers on it; the frozen pairs stay the local check on real data. | CI gets a real desktop-vs-FTS comparison with no personal data, under the same pairing guard (S5-1) plus a fingerprint of the rows. |
| P1-05-4 | Relevance pages come from one ranking of at most 1,000 posts per search and library state, cached by the server; its length is the total when it is shorter than the window. | §2.14 "inside a snapshot": pages never repeat or skip a post across writes, later pages skip the ranking, and most searches skip the count. |
