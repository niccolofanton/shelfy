# Shelfy — local continuation and current tracking

Snapshot time: 2026-10-03 18:26 CEST. Base fetched from GitHub: `web/foundations` at `abdd810` (27 commits after `86b5ab6`). This report supplements `EXECUTION.md`; it distinguishes implementation, verification and release.

| Area | Confirmed state | Next action / owner |
|---|---|---|
| Mac partial | `68c6c59` pushed to `web/p3-13-ai-drain`; 17 files, 1,912 added / 33 removed; pre-push Vitest 1,423/1,423 | Preserved as original recovery point |
| P3-13 | Local lane resumed from that work, rebased as `bbefe84` onto the current foundation | Codex engine lane: worker, routes, admin, resilient state machine, generated API and tests |
| F21 | Explicit library bearer read/write scopes not yet implemented | Codex scopes lane, no changes to F9 TTL policy |
| Tracking | GitHub updates fetched and local integration fast-forwarded; E19 recorded | Codex lead: reconcile and publish status without replacing cloud contributions |
| Existing cloud lanes | Still reserved by the execution log; process liveness not observable here | Do not duplicate; review their branch results as they arrive |
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

## Reserved cloud assignments

F9, F13, F17, F18, F20; P2-08, P2-14, P2-16; P3-02, P3-05, P3-06, P3-07, P3-18; P4-10, P4-11, P4-12, P4-13, P4-14, P4-16, P4-18, P4-22; UX-0.

Ownership is taken from the latest log, not inferred from a missing remote branch: an unfinished cloud lane may not have pushed yet. Re-read remote changes before integrating local work. Never force-push the shared foundation.

## Verification ledger

| Snapshot / check | Result |
|---|---|
| Original partial `68c6c59`, Mac pre-push | Vitest 112 files, 1,423 tests passed; Rust engine explicitly still incomplete |
| Foundation `abdd810` CI | Run `37135767275` failed: test, rust, web-e2e and web-e2e-live succeeded; blocking ssrf stopped because the fixture was unhealthy. Nonblocking Lighthouse LCP 4,433 ms exceeded 2,500 ms. F20 remains assigned to cloud |
| Local continuation | Required acceptance checks not yet completed; fill exact commit and outcome when lanes finish |

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
| P2-14 `c16c863` | F22 blocker: forged/stale upload completion and polling lease ownership require hardening | Resume submitted code in local review lane; do not mark complete or deploy unchanged |
| UX-0 `5b739eb` | Candidate ready for rebase and verification; Jobs target-size KNOWN list predates UX-6 | Reconcile stale known findings and run real-server UX suite before integration |
