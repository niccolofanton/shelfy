# X1 operator-node catalog benchmark — 2026-10-03

## Scope and acceptance

- Frozen owner gold: 40 posts, 38 complete and 2 login-gated. The stratified
  comparison uses the same 8 complete posts: 2 images, 2 carousels, 4 videos.
- Every operator-node request is serial, with no parallel benchmark or owner
  classification. No transcripts, schema changes or gold edits.
- Scorer v2 is fixed for every row: accents/trademark normalization, exact
  dotted/underscored `@handles`, entity micro precision/recall/F1, explicit
  false positives on empty entity gold, language weight 0.02.
- The 8-post candidate gate requires all 8 schema-valid, an improved composite
  and video score, and no material loss of entity precision. All regressions
  and failures remain in the results. Full-40 scores are an absolute check,
  not a comparative full-40 improvement claim.
- After the live inventory established that video objects are absent, a fair
  poster prompt comparison uses the original-prompt control with **identical**
  1024 px stills, six-image cap and hashtag trimming. Its composite 0.416,
  video 0.351, entity precision 0.833 and micro F1 0.556 are the poster control;
  the original extracted-frame baseline is a separate information-rich check.

## Results

| Run | Prompt digest | Valid schema | Composite | Image | Carousel | Video | Entity micro P / R / F1 | Total / max request ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Correct baseline, 8, extracted frames | `0a1795cbb407` | 8/8 | 0.455 | 0.508 | 0.527 | 0.391 | 1.000 / 0.500 / 0.667 | 460891 / 73680 |
| First candidate, 8, deep | `84f43a14b34f` | 8/8 | 0.467 | 0.609 | 0.463 | 0.398 | 0.722 / 0.542 / 0.619 | 581099 / 92451 |
| Conservative candidate, 8, poster | `5044eda8d891` | 8/8 | 0.381 | 0.485 | 0.384 | 0.327 | 0.846 / 0.458 / 0.595 | 516554 / 97300 |
| Original-prompt control, 8, poster | `0a1795cbb407` | 8/8 | 0.416 | 0.542 | 0.420 | 0.351 | 0.833 / 0.417 / 0.556 | 494137 / 91068 |
| Minimal candidate, 8, poster, trim | `ac46d33ae3fc` | 8/8 | 0.409 | 0.547 | 0.441 | 0.324 | 0.917 / 0.458 / 0.611 | 496703 / 87352 |
| Minimal candidate, 8, poster, weak | `ac46d33ae3fc` | 8/8 | 0.454 | 0.524 | 0.551 | 0.370 | 0.875 / 0.583 / 0.700 | 498718 / 96307 |
| Final candidate, all 40, poster, weak | `ac46d33ae3fc` | 38/40; 2 gated | 0.437 | 0.557 | 0.434 | 0.419 | 0.782 / 0.598 / 0.678 | 2421492 / 90585 |
| Final candidate, complete 38, poster, weak | `ac46d33ae3fc` | 38/38 | 0.460 | 0.557 | 0.474 | 0.437 | 0.782 / 0.610 / 0.685 | Same 38 requests |

- The first candidate **fails** the precision gate: 13/24 gold entities matched,
  18 predicted, 5 extra. Its small composite increase (+0.012) accompanies
  regressions in carousel quality, specific tags, flat tags, search keywords
  and entity F1. A full run was not started from this result.
- Private error audit: four extras occur in videos and one in an image; none
  occur literally in caption prose or are credited handles, and none are close
  spelling variants of gold entities. The revision restricts visual identities
  to clearly legible, relevant labels/credits and excludes guessed people,
  incidental UI/background names and generic headings.
- The revised poster candidate also **fails** the gate: 11/24 entities matched,
  13 predicted, two extra, and lower tag scores. It was not expanded to 40.
  The original-prompt control with identical poster inputs scores 0.416, so
  the conservative rewrite itself regresses by 0.035. The minimal revision
  (`ac46d33ae3fc`) retains the original catalog rules. Its precision and micro
  F1 improve, but composite (-0.007) and video (-0.027) still regress against
  the fair poster control; it was not expanded to 40.
- The deep and poster candidates use different prompt
  versions as well as different media, so they are not a pure media ablation.
- Caption evidence audit: 16/24 gold entity names occur literally in the original
  captions, only 12/24 after hashtag-wall removal. The four lost names remain
  part of the frozen gold; this recall cost is disclosed rather than removing
  difficult gold labels. The final engine policy preserves them as weak data.
- Retaining hashtag walls as weak data (`captionPolicy=weak`) with the minimal
  prompt and identical poster inputs **passes the fair 8-post gate**: composite
  +0.038, video +0.019, precision +0.042, recall +0.166 and entity micro F1
  +0.144 versus the original-prompt poster control. Fourteen gold entities match,
  sixteen are predicted, two extra. Compared with the same minimal prompt and
  trimmed caption it recovers three gold entities and +0.045 composite.
- This passing gate supports the combined prompt/caption-policy change. The
  comparison to the original poster control changes both prompt and caption
  policy; the two minimal-prompt rows isolate the caption-policy effect.
- Image score regresses by 0.018 against the poster control; search keywords
  improve by 0.006, while specific tags remain lower by 0.002. These losses are
  retained. The engine selection and runner defaults now preserve the original
  caption as weak evidence.
- Full run completed: 38 schema-valid answers, zero invalid answers/errors,
  two gated posts without requests. It sent 81 still images over 38 serial
  requests, taking 2440626 ms wall time. Frozen gold SHA-256 and configured
  model match the comparisons; prompt digest is `ac46d33ae3fc`.
- On all 40, entities match 61/102 expected with 78 predicted; gated posts are
  scored as missing. On the 38 complete cases, 61/100 expected match, with
  17 extras across 10 posts and 39 missed names. Empty entity gold produces
  zero false-positive posts: 0/7 on all 40, 0/6 on complete cases. These are
  absolute results without a full-40 baseline or claim of full-40 improvement.
- Complete-case field means: general tags 0.587, specific tags 0.315, flat tags
  0.464, entity macro F1 0.713, search keywords 0.417, description 0.301,
  save reason 0.231, language 1.000. Specific detail and entity recall remain
  limited; a passed 8-post comparison does not mean every full-run case is good.

## Error audit

- Broad subject terms remain present in the poor curated-reference and modular
  construction cases. Their low lexical scores also reflect missed concrete
  details and identifiers; they are not all gross subject substitutions.
- Four of the first deep candidate's five extra entities occur in one
  curated-reference video. Poster-only inputs suppress those additions but
  cannot expose every work, credit or detail that occurs later in a video.
- Typography and interaction-tutorial cases also lose specific detail with
  the conservative poster rewrite. These findings retain every poor case and
  motivated the minimal prompt instead of tuning to particular gold names.
- A direct visual spot-check of five low-scoring full-run cases examined their
  five covers and three additional fashion-carousel stills. Fashion, construction
  and collage subjects remain represented; concrete textile/technique details
  and named references are missed. A readable series title is preserved. One
  entity counted as extra by frozen gold matches a readable slide label, so that
  scoring extra is not evidence of an invented identity. Gold remains unchanged.
- The lowest complete case has a mostly blank logo cover. It scores 0.149 and
  misses five named references; that single still does not expose the video's
  later subject matter. Across the five lowest complete cases, scores range
  0.149–0.227 and 16 gold entities are missed, with only one scoring extra.
- Across the complete run, nine of seventeen scoring extras occur literally in
  the original captions. Fourteen missed names also occur in the original
  captions, and all fourteen survive in the bounded caption actually sent.
  Literal presence is a support check, not proof that a name is relevant to
  the catalog; these remaining recall misses are not explained by truncation.
- The private audit checks topic terms, caption occurrences and gold entity
  matches alongside this limited still review. It is not a complete human
  semantic/video review and does not certify all extra identities as readable.

## Media and production limits

- Correct baseline: original prompt; 448 px JPEGs; up to four spread extracted
  video frames and eight carousel images. Actual 8-post image counts:
  `1, 1, 7, 7, 4, 4, 4, 4`.
- Deep candidate: cover first and image-byte digest deduplication, at most eight
  slides, video poster at `min(duration × 0.1, 1s)`, then up to four equal-span
  midpoint keyframes, six JPEGs total at 1024 px. All 40 preflight: 204 images
  over 38 complete posts; the two gated posts make no request.
- Poster candidate: existing cover, image slides and exported video posters
  only; no video decoding, downloads or substitute keyframes. The same six-image
  cap and 1024 px still size apply. All 40 preflight: 81 images over 38 complete
  posts, none without a still.
- Inputs are an independent private extraction (images originally up to
  1600 px, eight extracted 960 px frames per video, and downloaded MP4s), not
  production CAS. Deep candidate uses MP4 decoding; its JPEG conversion differs
  from the engine's WebP-to-JPEG path. It does not establish production parity.
- The live owner's library has **zero linked video objects**. Deep benchmark
  quality cannot gate its first catalog run. Poster mode represents available
  media more closely; production must still verify its own CAS objects and
  readability. All complete benchmark cases contain a still, so this run does
  not validate production posts that have neither a cover nor an image slide.
  No mass video download is authorized by this benchmark.
- To match the benchmark's 1024 px still size, launch the engine with
  **`deep=true`** while video objects remain absent: it sends only existing
  stills, without video downloads. Default `deep=false` sends 480 px stills and
  is a different media-size profile. Caption policy now matches both products.
- The earlier `baseline-1854` run returned 8 schema-valid answers but sent zero
  images for every sampled video: extracted frame names were resolved outside
  their `slide-0/` directory. Its composite 0.387 is retained as a faulty
  caption-only diagnostic and excluded from media quality comparisons. The
  still earlier `baseline-1848` also used incorrect catalog builder arguments
  and is excluded entirely.

## Changes and verification

- The runner resolves nested frames, fails missing/escaping declared media,
  decodes all selected media before the first node request, and records actual
  counts plus pipeline/media mode and prompt digest. Existing run directories
  and symlink paths back into any Git checkout are refused; private directories
  and JSON files have permissions 0700 and 0600.
- Candidate prompt separates visible subject/function from promotional calls
  to action; treats hashtags as weak evidence; includes identified works,
  titles and explicit creator credits; preserves handle spelling; forbids
  tools or creators inferred solely from style/uncorroborated hashtag lists.
- Shared index, Rust golden requests and desktop recordings regenerated.
  Recording diff reviewed: only system/user text in seven social catalog
  cases changed; schema, model settings and attached media remained unchanged.
- Verification after the final foundations rebase: 92 TypeScript tests;
  `cargo test -p shelfy-core` with 460 passed
  and 2 previously ignored; golden `--check`; Node/test TypeScript checks;
  targeted ESLint, formatting and diff whitespace checks.

Personal gold, captions, answers, media, provider endpoints and credentials stay
outside Git; this report contains aggregates only.
