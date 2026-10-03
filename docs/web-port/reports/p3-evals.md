# P3 pipeline evaluation

## P3-24 synthetic pipeline contract

- Cases: the fixed 20 in `scripts/extract-eval/cases.json` (10 videos, four
  carousels, three images, three text posts); extraction gold/scorer unchanged.
- Stub answers derive from existing gold. Their recorded composite **1.000**
  checks pipeline transport and normalization only; it is not model quality.
- Catalog digest: `ac46d33ae3fc`, approved through the real
  [X1 benchmark](x1-node-benchmark-2026-10-03.md). No new prompt change.
- The Rust integration test drives the actual catalog worker through queue,
  DB input selection, synthetic CAS, image rendering, loopback provider and
  normalized writes. Profiles: shallow `g480`/480 and approved still 1024.
  These are maximum edges; the engine does not upscale smaller CAS sources.
  Video fixtures use stored posters without `video_object_id`, matching the
  approved current-CAS production profile. Keyframe extraction and audio are
  not covered by this stub contract.
- Checks include cover/slide deduplication, source ordering, six-image cap,
  JPEG transport, weak caption evidence, request settings and raw equality.
  Each profile declares 57 CAS stills and expects 35 selected JPEGs; three
  cases exceed the six-image cap. The text-only cases send no images.
  Missing-poster videos remain `waiting_for_media` and are not enqueued;
  a declared but unreadable CAS poster fails with `media_unreadable`. Both
  regressions require zero catalog calls, preventing caption-only conversion.
- Request audit: engine and X1 both use strict JSON schema, shared sampling
  settings and `enable_thinking=false`. The engine streams with usage metadata
  while X1 uses a non-streamed response. The engine also supplies the library's
  top-30 vocabulary, refreshed by generation; the independent X1 harness sends
  no vocabulary hints. These tests exercise the actual evolving DB vocabulary,
  but canned answers cannot measure its impact on live quality.
- Real owner content, captions, provider responses and credentials are not
  committed by this lane. No live inference is run during the owner queue.
- Validation: four Rust integration tests pass: 20 cases per still profile,
  missing-poster queue gating and unreadable-CAS worker failure. Four
  TypeScript parity tests, fixture regeneration check, Node/test type checks,
  lint, Rust formatting and targeted Clippy with `-D warnings` pass.
- The local real-mode adapter, 20-case live engine run and compatible desktop
  comparison remain outstanding; the X1 independent-media benchmark does not
  establish complete production pipeline parity or satisfy this separate
  comparative gate.
