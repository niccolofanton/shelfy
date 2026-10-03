# Shelfy — local continuation and current tracking

Historical snapshot: 2026-10-03 19:23 CEST. See the latest checkpoint below before using these historical rows as current state. Integration branch `web/foundations` is now `cc4f594`: P2-14/F22 and F20 integrated after the fetched `abdd810` base and continuation documentation. Publication and remote CI are recorded separately below. This report supplements `EXECUTION.md`; it distinguishes implementation, verification and release.

| Area | Confirmed state | Next action / owner |
|---|---|---|
| Mac partial | `68c6c59` pushed to `web/p3-13-ai-drain`; 17 files, 1,912 added / 33 removed; pre-push Vitest 1,423/1,423 | Preserved as original recovery point |
| P3-13 | Local lane resumed from that work, rebased as `bbefe84` onto the current foundation | Codex engine lane: worker, routes, admin, resilient state machine, generated API and tests |
| F21 | Code ready at `6f651ac`; read/write scopes and authz verified | Hold control migration 0006 until P4-11 reserved control 0005 lands; parent then rebases and refreezes v6 |
| Tracking | E19–E22 recorded; complete implementation inventory requested by the owner | Codex lead maintains integration, evidence and the inventory |
| Existing cloud lanes | Claude credits exhausted; all remaining tasks taken over under E20 | Published P2-14 and UX-0 code recovered; unpublished tasks resume from cards in isolated worktrees |
| Release | Last recorded production release remains rc.5 | Next server release only after required checks and actual deployment verification |
| Live work | F12 backfill, AI provider setup/gold gate/first run, P1-26 and personal sync checks remain in `reports/live-steps.md` | Local lead can reach local data and infra; execute in dependency order |
| Full IG tagging | Not started; a compiled provider service alone is insufficient | Wait for complete P3-13 and measured gold pass |

## Newly integrated results

| Task | Implementation commit(s) in foundation | Current limit |
|---|---|---|
| F8 | `0cb019d`: media duration histogram has variant label | Needs deployment before live g480 budget can use it |
| F12 | `814a031`: migration preserves palette/font/tech/awards | Existing owner/mock site capture requires separate live backfill |
| F16 | `d61e89e`: response schema preserves source key order | Use RawValue path when constructing P3-13 requests |
| F19 | `d305f7d`: single-job cancel/retry refresh queue summary | Actual release verification pending |
| F15 | `e59998f`: purpose-specific connect timeout and offline classification | Drain itself must pause on quota exhaustion |
| F10 | `27a4f7b`, `170b30e`: reauth does not consume sign-in budget; Approve gated | Device flow must stay protected in future UX work |
| P2-12 | `ffa06ba`: Connections settings, pairing/status/Shortcut | Requires unpacked extension and a real-session check |
| P2-13 | `262bf7e`, `139c480`, `512f96c`, `227f9cf`: sync controller, worker, UI and synthetic smoke | Real IG/X/Pinterest owner syncs still pending |
| UX-4 | `76ef930`: fallback card, selection overlay, touch area and alt | Integrated does not imply production deployed |
| UX-6 | `035fe2b`: Trash/Jobs primitives, layout, queue menus and progress | Integrated does not imply production deployed |

## Claude tasks taken over under E20

F9, F13, F17, F18, F20; P2-08, P2-14, P2-16; P3-02, P3-05, P3-06, P3-07, P3-18; P4-10, P4-11, P4-12, P4-13, P4-14, P4-16, P4-18, P4-22; UX-0.

The owner confirmed Claude credits are exhausted and asked Codex to continue these tasks. P2-14 and UX-0 have recoverable submitted branches; P3-13 has the preserved Mac partial. Other unintegrated task branches were not present in the fetched remote inventory. Recover any additional published work before implementation; do not claim that unpublished remote code was recovered. Never force-push the shared foundation.

## Verification ledger

| Snapshot / check | Result |
|---|---|
| Original partial `68c6c59`, Mac pre-push | Vitest 112 files, 1,423 tests passed; Rust engine explicitly still incomplete |
| Foundation `abdd810` CI (historical) | Run `37135767275` failed: test, rust, web-e2e and web-e2e-live succeeded; blocking ssrf fixture unhealthy. F20 is now fixed locally. Nonblocking Lighthouse LCP 4,433 ms exceeded 2,500 ms; F18 remains open |
| P2-14 + F22 `30b4e97` | Integrated after parent review: 64 targeted Rust tests; clippy, fmt, generated API/client and web typecheck green. Required leaseId, final eligibility/lease fence, bounded idempotency and actual-byte quota |
| F20 `cc4f594` | Integrated after parent review: IPv6 cold-start 3/3, SSRF 92/92 (89 refusals + 3 controls), video 38/38 with ffmpeg, media clippy. Earlier Linux reproducer improved 10/200 failures to 0/500. Remote CI not yet rerun |
| P3-13 work in progress | 500-post acceptance including offline mid-run, pause/resume, restart and refusal retry passed; final workspace/concurrency checks and integration remain |
| X1 corrected preflight | The first eight-answer baseline had video frames missing because nested paths were unresolved; excluded from media-quality comparisons. New baseline resolves every input and validates media counts before the model call |

## Fetched commit inventory

| Commit | Original subject |
|---|---|
| `6cd9f36` | docs(web-port): move the lead to a cloud session and resume at full parallelism |
| `29c6e6b` | docs(web-port): start the cloud wave: F8–F20 and waves 1–2 of the open work |
| `0cb019d` | feat(server): add a variant label to the media route's latency (F8) |
| `814a031` | fix(migrate): carry site palette, fonts, tech and awards into web captures |
| `5ecb105` | docs(web-port): mark F12 done |
| `d61e89e` | fix(ai): send the response schema in the file's key order |
| `7b4320a` | docs(web-port): mark F16 done, add its note for P3-13 |
| `76ef930` | feat(ui): polish post cards (fallback overlap, selected ring, hit area, alt) |
| `4a79f26` | docs(web-port): mark UX-4 done |
| `d305f7d` | fix(jobs): refresh the queue summary after a single-job cancel or retry |
| `b940f6f` | docs(web-port): mark F19 done |
| `ffa06ba` | feat(web): add Settings connections for extension pairing, status and Shortcut |
| `abbf3d6` | docs(web-port): mark P2-12 done |
| `ca8df68` | docs(web-port): start P2-08, P2-16, P3-02, P3-18, P4-12, P4-16, P4-18, P4-22, UX-6 |
| `f2b9e91` | docs(web-port): record the suspended P3-13 lane and the steps for the first IG run |
| `bd4bc73` | docs(web-port): record E18, no new lanes until the running ones finish |
| `e59998f` | fix(server): treat a connect timeout as a connect failure, per-purpose connect timeouts (F15) |
| `5c1281e` | docs(web-port): mark F15 done, add its notes for P3-13 and P3-19 |
| `27a4f7b` | fix(server): do not count reauth_required answers against the sign-in limit |
| `170b30e` | fix(web): keep Approve off on /device until the re-authentication succeeds |
| `035fe2b` | feat(web): polish Trash and Jobs with shared primitives and mobile layout (UX-6) |
| `1bdfa25` | docs(web-port): mark F10 and UX-6 done |
| `262bf7e` | feat(extension): add the sync controller in the syncing tab (P2-13) |
| `139c480` | feat(extension): open, feed and close explicit sync runs in the worker (P2-13) |
| `512f96c` | feat(extension): add "Sync now" and the folder chooser to the side panel (P2-13) |
| `227f9cf` | test(extension): add the sync smoke test on synthetic pages (P2-13) |
| `abdd810` | docs(web-port): mark P2-13 done, add its notes and the first real sync checks |

## Review of submitted cloud branches

| Branch | Review status | Action |
|---|---|---|
| P2-14 `c16c863` | Original F22 findings resolved and integrated as `30b4e97` | P2-17 must echo polled leaseId; browser worker/live verification are separate tasks |
| UX-0 `5b739eb` | Submitted code recovered and rebased; synthetic/real-server verification in progress | Reconcile stale Jobs target-size and contrast findings, preserving strict known-finding checks |

## Implementation inventory and current assignments

The owner requested implementation status, not time estimates. The complete inventory is [implementation-inventory-2026-10-03.md](implementation-inventory-2026-10-03.md): 115 P1–P4 cards, with 51 integrated, 16 pending (in progress, ready or taken over), 47 not started and P1-27 dropped. Follow-ups, UX, desktop SaaS, MCP and operational exit work are listed separately rather than hidden in a percentage.

After the owner raised the cap to ten subagents and restarted Codex, all lane worktrees survived. Current parallel assignments use GPT-6.1 Sol / high: P3-13 completion, X1 harness tuning, P4-11 export, P2-08 Activity, P2-16 selection, P3-02 vault, P3-05 tag ranking, P3-06 clusters/aliases, UX-0 harness and the implementation inventory. F20 finished and its slot moved to X1.

Under E22 the immediate operational priority remains the tagging chain: correct the real-node benchmark, improve shared prompts/media selection, finish and verify P3-13, deploy with operator concurrency one, then queue all saved Instagram posts for the owner's profile. The production CAS/video coverage must be measured separately from the privately fetched benchmark media. No full owner tagging run has started.

## Checkpoint after the parallel-lane audit, 2026-10-03

- Integration source `3a03d2c`, documentation `d7219ae`, then CI-only fix `56d82ee`. The full locked Rust workspace run completed with exit 0: **1,654 passed, 8 ignored across 114 suite summaries**. Private local evidence: `shelfy-web-local/data/release-candidate-rust-2118.log`.
- Publication of `d7219ae` was stopped by the normal pre-push hook: **1,746 passed, 2 failed** in `tests/shared-ai/eval-run.test.ts`. Both concern Git worktree detection for private benchmark output directories. The harness lane is correcting the subprocess environment; the privacy guard must remain enforced. This is not a successful push.
- CI fix `56d82ee` builds the deterministic AI stub before real-server Playwright. Previous run `37146987723` failed before executing browser specs because that binary was missing; the fix still needs remote CI.
- Ten lanes were checked and responded with concrete progress. P3-14 is ready at `c20e7ad` (38 server and 56 core tests reported after rebase); its agent proceeds to P3-15 UI. P4-13 is ready at `8419d5c`, with full compose isolation on Docker >=28 still unverified. P3-27 checkpoint `42c5c21` has its three exact server tests passing; rich design parity continues. None of these pending commits is described as deployed.
- Shared Git `core.bare=true` interrupted linked worktree discovery. Local `git config --worktree core.bare false` restores each affected lane without changing the common configuration.
- Disk reached 546 MiB free. The P3-14 agent removed only its generated target after confirming no active handles; the lead verified **7.6 GiB free** afterward. Large parallel builds remain restricted; GC/export gets one targeted rerun.
- Production remains rc.5; owner tagging has not started. The completed X1 benchmark, live backup evidence, next CI gate, 81-poster recovery and actual owner run remain distinct steps.
