# Shelfy online — execution log

Live status of [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md). The plan is the spec; this file records how the work runs, what is done, and what waits on the owner.

## How the work runs

- **Integration branch:** `web/foundations`, branched from `dev`. Every task lands on it as a fast-forward and is pushed; CI must be green.
- **Lanes:** one task per lane. A lane is a Claude Code subagent in its own git worktree, on branch `web/<task>-<slug>`, started from the tip of `web/foundations`.
- **Lead:** one session plans the waves, reviews each lane (diff, checks, and an independent reviewer for substantive tasks), integrates it and updates this file.
- **Concurrency:** a new lane starts only while 8 GB of local disk stay free. Even with line tables only in dev builds (7ba4ea2), a Rust lane's target dir reaches 3.5–6 GB, 1.3–2.5 GB of it incremental, so lanes started after 2026-10-02 18:10 build with `CARGO_INCREMENTAL=0`.
- **Budget:** the lanes and the lead share the owner's Max plan. The lead starts no new lane once the weekly all-models limit passes 80 %; lanes already running finish, and new work resumes after the weekly reset. New lanes use Opus for security, data, backups and architecture, and Sonnet for UI and for fixes already specified in detail; reviews and tests check both the same way. The owner can change either rule. On 2026-10-03 the owner asked for as many lanes in parallel as possible and raised the limit to 90 % of the weekly all-models limit.
- **Restarts:** lanes run inside the lead's process. Anything that restarts it, such as a permission-mode change, stops every running lane. Worktrees and transcripts survive: the lead resumes each lane with a message, as on 2026-10-02 at 18:05.

## Lane rules

1. Read the plan sections your task cites before writing code. The plan is the spec.
2. Branch from the integration tip: `git switch -c web/<task>-<slug> web/foundations`.
3. Run `pnpm install --frozen-lockfile` once; it installs the git hooks and lets you run the desktop checks.
4. Rust uses the toolchain pinned in `rust-toolchain.toml`. For the desktop e2e, rebuild with `npx electron-rebuild --force --only better-sqlite3`, because the plain command silently does nothing. Afterwards run `pnpm rebuild better-sqlite3` so vitest works again. Run cargo with `CARGO_TARGET_DIR="$PWD/target"`: with a shared target dir, lanes overwrite each other's test binaries.
5. Scope is additive (D25): the desktop app (`electron/`, `src/`, root build config) stays untouched unless the task says otherwise. Do not change the root `package.json` or `pnpm-lock.yaml` unless the task needs a dependency, and say so.
6. Commits: Conventional Commits in English, header ≤ 100 characters, no `Co-Authored-By` trailer. Stage explicit paths, never `git add -A`, never `--no-verify`, never `git stash` (the stash is shared across worktrees). Do not push or merge.
7. Before finishing, rebase on `web/foundations` if it moved, re-run the checks, and leave the worktree clean.
8. When the plan does not cover a decision, pick the plan-consistent option and report it as an assumption. Do not stop.
9. Privacy: the reference library snapshot (outside the repo, in `../shelfy-web-local/ref/`) is the owner's personal data. Read it read-only and never commit its content; fixtures are synthetic or scrubbed, and only aggregate counts appear in docs.
10. A lane that runs a server uses its own data dir (`../shelfy-web-local/data/<task>`) and port `18080 + n`, where `n` is the task number.

## Decisions during execution

These owner decisions override the plan where they conflict.

| # | Date | Decision | Replaces in the plan |
|---|---|---|---|
| E1 | 2026-10-02 | No throwaway servers. SPIKE-2, 4, 10 and 11 and the P5 capacity run happen on the osn VPS itself, inside resource-limited containers. | D20 "throwaway CX33s"; §6.3; §9 methods |
| E2 | 2026-10-02 | The lead may deploy, restart and reconfigure on the osn VPS without asking. The one hard constraint: Hermes, the AI agent, must keep working. Restarting it is fine. | Appendix B sign-offs |
| E3 | 2026-10-02 | The extension is tested as an unpacked build. No Chrome Web Store submission for now. | SPIKE-7; the store steps in P2 and P5 |
| E4 | 2026-10-02 | Owner-only until further notice: no invites and no closed beta. The owner account is created with the admin CLI; its email lives in the osn secrets, not in this repo. | P2 closed beta; P5 invites |
| E5 | 2026-10-02 | The web app is served at `refs.niccolofanton.dev`. Done on 2026-10-02 (osn `ebd2de8`): the CNAME, an owner-only Access app, the tunnel ingress and the edge block to `shelfy-api:8080` are live, so the host answers 502 until the service is deployed. The lead sets up the DNS record, the tunnel route, Access and the edge nginx route on Cloudflare and the VPS. | `shelfy.niccolofanton.dev` throughout the plan |

| E6 | 2026-10-03 | A mock account on the live host, seeded with about 500 of the owner's posts, is the test account: lanes may sign in to it in automation (unlike the owner's, lane rule 8). Every web feature is tested end to end on it before the port is called done. | §6.1 test data; P1 lane rule 8 for this account only |
| E7 | 2026-10-03 | The desktop app becomes a client of the server: it signs in to an account and works on the server's library; the in-app browser sync and downloads feed the server like the extension does. It keeps a local AI model for cataloging. Every feature is tested in the desktop app with the mock account too. | §2.20 and P6 (a local library on the shared core through napi-rs) |
| E8 | 2026-10-03 | The mobile UI is optimized, and UX/UI design experts review the whole app and improve it to a professional standard without redesigning it. | — |
| E9 | 2026-10-03 | The owner's AI node may be woken when the tests need it, as Hermes does with `/wake`. | L15's "never wakes it" |

### Lead decisions

Changes to the plan that the lead made during execution, with the reason.

| # | Date | Decision | Replaces in the plan | Why |
|---|---|---|---|---|
| L1 | 2026-10-02 | Sign-in links are `<public URL>/login/magic#<token>`. The SPA page redeems them with `POST /auth/magic-links/redeem` after a click; there is no GET route that signs in. | §2.9 `GET /auth/magic/{token}` that sets the cookie and redirects | Mail scanners and link previews fetch URLs and would spend the link or sign themselves in; a token in the path lands in the nginx and Cloudflare logs (T10 security review, M1). |
| L2 | 2026-10-02 | Authentication is deny-by-default. A route answers 401 unless it is listed in `routes::PUBLIC_ROUTES` or `routes::TOKEN_ROUTES`; the authz test pins the OpenAPI document to those lists. | Per-route opt-in through the `CurrentUser` extractor | A route that forgets the extractor, or one missing from the document, would ship public (T10 security review, L5). |
| L3 | 2026-10-02 | `CF-Connecting-IP` is trusted only when the TCP peer is inside `SHELFY_TRUSTED_PROXIES`; IPv6 clients are rate-limited per /64. | The header was trusted from any peer | Any container on the Docker network could set the header, and IPv6 clients rotate addresses inside their /64 (T10 security review, L3). |
| L5 | 2026-10-02 | The `shelfy-api` image keeps Debian's `ffmpeg` and is 623 MB on arm64: ffmpeg about 400 MB, 153 MB of it Mesa and LLVM pulled in through libavdevice; yt-dlp 98 MB; base 97 MB; server 24 MB; web app 4 MB. | §3.2 estimate of ~180 MB | ffmpeg parses untrusted media, so Debian security updates matter more than size. Removing packages leaves dpkg broken, and a static ffmpeg gets no security updates. Revisit in P5. |
| L6 | 2026-10-02 | The image ships yt-dlp's unpacked `yt-dlp_linux.zip` build, pinned by SHA-256 per architecture. | The one-file `yt-dlp_linux` binary | The one-file binary unpacks about 100 MB into `/tmp` on every run, and the container's tmpfs is `noexec`, so it fails to load `libz`. |
| L7 | 2026-10-02 | Passkeys use webauthn-rs 0.5.5 with a vendored, statically linked OpenSSL 3. Tests use our own software authenticator in `tests/support/passkey.rs`. | §2.21's "no OpenSSL" | The only pure-Rust webauthn-rs is a 0.6 pre-release whose `rsa` dependency fails cargo-deny (RUSTSEC-2023-0071). Other pure-Rust relying-party crates are young or unmaintained. Vendoring needs no system OpenSSL at runtime, and cargo-deny tracks the OpenSSL version. |
| L8 | 2026-10-02 | Passkeys require user verification and discoverable credentials. Adding a passkey, like removing one, needs a sign-in or re-auth from the last 5 minutes. Registration is `POST /me/passkeys/start` then `POST /me/passkeys`. | UV "preferred" (card and §2.11); recent auth only for removal; §2.9's `/auth/passkeys/register/{start,finish}` | webauthn-rs's passkey API enforces UV, so re-auth really checks the user, and every P1-24 platform verifies the user anyway. Adding a sign-in credential deserves the same check as removing one. |
| L9 | 2026-10-02 | `POST /auth/device/poll` is exempt from the shared sign-in limit (10/min per client) and capped at 20 polls a minute per device code. `start` and `poll` are exempt from CSRF, read no cookie, and are listed in `CSRF_EXEMPT_ROUTES`, where a test checks that every route is public. | P1-15's single `/api/v1/auth/*` sign-in limit | The CLI and the approving browser usually share an IP, so polling would use the whole budget and the browser's re-auth and approval would get 429. The CLI sends no `Origin`. |
| L10 | 2026-10-03 | Lanes that change the HTTP API may run at the same time. The later lane rebases, unions the registry additions, takes the next free migration number and regenerates `openapi.json` and the TS client with the tools. | P1 lane rule 2, "no wave pairs two such lanes" | The owner asked for maximum parallelism, and fast-forward integration already makes every lane rebase on the tip before it lands. |
| L11 | 2026-10-03 | P2-04 owns the one outbound HTTP client, in `crates/server/src/outbound/`, and absorbs P4-01: purpose-based clients, `SHELFY_EGRESS_PROXY` when set, `internal()` for the capture service only, the redirect policy (≤ 5 hops, http(s), ports 80 and 443), byte caps, ≤ 64 requests in flight, no cookies, the test that refuses a `reqwest` client built elsewhere, and `shelfy_egress_requests_total{purpose,outcome}`. Without a proxy, its resolver refuses private, loopback, link-local, CGNAT, ULA and multicast addresses. P4-02 builds the proxy and the SSRF suite on it. | P4-01 as a separate `egress.rs` | Both plans defined the same client; one module avoids two egress paths. |
| L12 | 2026-10-03 | P4-08 owns tus uploads with sessions, the `uploads` scope and the purpose registry. P2-14 adds the extension purposes and needs P4-08. | P2-14 building its own upload access | One upload path. |
| L13 | 2026-10-03 | P4-07 owns quotas and reservations. P2-10's archive drain reserves through it and leaves an item `link_only` when refused, so P2-10 needs P4-07. | P2-10 without quotas | The archive is the largest writer of media. |
| L14 | 2026-10-03 | P4-09 (Jobs view) owns the jobs seam, hook and `jobs` messages, and lands before P2-08 (Activity center), which reuses them. | P2-08 and P4-09 in parallel | Both read jobs; parallel lanes would write two seams. |
| L15 | 2026-10-03 | The owner's AI node is P3's test target and the owner's default provider: an OpenAI-compatible endpoint (`ornith-1.5-35b-a3b`) and a whisper.cpp server on the owner's PC, reached over Tailscale from the VPS and the Mac. The server defines it from env (osn SOPS), never shows its key, and allowlists exactly that host and port; user-entered base URLs keep the strict SSRF rules. Shelfy uses it gently (1–2 requests at a time, a breaker) because Hermes shares it, and never wakes it. | §2.15 BYOK only; SPIKE-6 on cloud providers | Owner decision, 2026-10-03: "use my custom AI endpoint on the remote node for all the tests and pre-configure it". Cloud keys become optional. |
| L4 | 2026-10-02 | P1-04 creates `web/playwright.config.ts` for its deep-link smoke test; P1-02 extends it. | P1-02 owns the file "if T12 left none" | T12 left none, and P1-04 runs before P1-02 because both edit `src/App.tsx`. |

## Status

### P0 — Foundations and spikes

| Task | What | Needs | Status | Branch |
|---|---|---|---|---|
| T1 | Cargo workspace, pinned toolchain, `deny.toml`, `deploy/` skeleton, CI `rust` job | — | done | `web/t1-workspace` (b0baaf5…bad7139) |
| T2 | SPIKE-1: legacy reader, canonical keys, `shelfy-migrate plan` | T1 | done | `web/t2-legacy-reader` (dc6f5e7…a4d2c7c) |
| T3 | Schema v1, `UserDb`, repositories, FTS maintenance, golden harness | T1 | done | `web/t3-schema-v1` (ec1d742…5362bc0) |
| T4 | SPIKE-5: FTS relevance against `search-eval` | T3 | done | `web/t4-fts-relevance` (9f0f4d3…37b31d0) |
| T5 | SPIKE-3 build: minimal MV3 extension and comparison tooling | — | done (owner run pending, O1) | `web/t5-extension-spike` (a35f4fe…6b8dbf4) |
| T6 | SPIKE-2 and SPIKE-10 on the osn VPS (E1) | — | done | `web/t6-spikes-vps` (861e361, 5e3b64f) |
| T7 | `crates/server`: axum app, config, health, metrics, errors, OpenAPI, admin CLI | T3 | done | `web/t7-server` (f95b212…bfb0dbc) |
| T8 | `crates/media`: CAS, renditions, ThumbHash, `/media/*` | T7 | done | `web/t8-media` (6dd9433…1ddb6d9) |
| T9 | Migration v0 and the reference library installed locally | T2, T3, T8 | done | `web/t9-migration` (db57b99…567fbf1) |
| T10 | Owner auth v0: magic link, sessions, CSRF | T7 | done | `web/t10-auth` (c7d33ae…03a6cf8) |
| T11 | Read API and generated TS client | T4, T7 | done | `web/t11-read-api` (fc1c3b7…9b0f643) |
| T12 | SPA slice behind `ShelfyClient` | T10, T11 | done | `web/t12-spa` (0a54f97…a1315a0) |

**P0 exit check (2026-10-02, plan §10), on the integration tip with a release build and the reference library installed by T9.**

| Criterion | Result |
|---|---|
| The migrated reference library is browsable and searchable in the SPA | Pass. 6,138 posts installed (IG 3,997, X 2,140, web 1), 5,379 objects, 4,545 g480 renditions. Signed in through `/login/magic`, then browsed the gallery, the folder, search and the post modal. At 390×844 the layout is still the desktop one; P1-02 makes it responsive. |
| List p95 ≤ 40 ms server time | Pass: 203 list requests (all 103 pages plus filtered views) p50 1.2 ms, p95 2.6 ms, measured end to end on localhost. Search p95 4.5 ms over 100 queries; stats p95 9.0 ms. |
| Search passes the SPIKE-5 gate | Pass (T4, on the frozen pair). |
| Spike notes 1, 2, 3, 5 and 10 committed with their decisions | 1, 2, 5 and 10 committed. SPIKE-3 waits for the owner run (O1); its tooling is done. |
| Open questions for week 3 | Passkeys: P1-13 and P1-24. First osn PR: P1-16. Chrome Web Store: dropped (E3). |

### P1 — Library on the web

27 tasks in 13 waves: [phases/P1.md](phases/P1.md). A task starts as soon as the tasks it needs are integrated.

| Task | What | Status | Branch |
|---|---|---|---|
| P1-01 | Realtime: SSE bus, notifications, client errors, version | done | `web/p1-01-realtime` (e7afc2c…86f34ca) |
| P1-07 | Job system and jobs API | done | `web/p1-07-jobs` (e9fce3a…9499f3c) |
| P1-12 | Backup, restore and schema-upgrade tooling | done; the independent review found 2 high, 5 medium and 7 low issues, fixed in F4 | `web/p1-12-backup` (6989b13…61da797) |
| P1-09 | Deployable server: SPA hosting, headers, image, release workflow, `compose.test` | done | `web/p1-09-deploy` (0f35249…04f00a5) |
| P1-13 | Owner passkeys, re-auth, login-link bootstrap, optional SMTP | done | `web/p1-13-passkeys` (fcd5cd7…c887a3c) |
| P1-03 | Library writes, folders, selector, stats, ETags | done; the independent review found no critical or high issue, 1 medium (fixed in P1-05) and 6 low (fixed in F6) | `web/p1-03-writes` (cde994f…232539f) |
| P1-04 | Client seam: routes, SSE client, error boundary, error codes | done | `web/p1-04-client-seam` (4f16f8e…b4afd04) |
| P1-10 | Merge rules in the core and golden parity | done | `web/p1-10-merge` (ab6efbd, 2364d64) |
| P1-05 | Search and filters complete, search-eval gate, `admin synth`/`bench` | done | `web/p1-05-search` (d5e5df9…6cb1eef) |
| P1-15 | Metrics, log redaction, rate limits | done | `web/p1-15-observability` (8ef7557…3ce52ee) |
| P1-17 | Account API, API tokens, device-code flow | done | `web/p1-17-account` (47bbab0…64b9241) |
| P1-02 | Responsive shell and a minimal web app manifest | done (Sonnet) | `web/p1-02-responsive` (960e246…33e7c5e) |
| P1-18 | First osn PR, part 2: apply stage 1 (services) | done on 2026-10-02 at 23:20: [osn PR #29](https://github.com/niccolofanton/osn/pull/29) merged, `server-v0.1.0-rc.1` (fa3b286) deployed. `shelfy-api` is healthy and `refs` answers through the edge, behind Access. The edge subnet is `10.91.0.0/24` and equals `SHELFY_TRUSTED_PROXIES`. `up{job="shelfy"}` is 1. A restart is healthy within 1 s. Owner created, and the owner signed in through Access and a sign-in link. Hermes is unchanged: same start time, 0 restarts, 2 GiB / 2 CPU, node status online. Backups are still off. | osn `main` fa2066e |
| P1-19 | Migration tool and install job complete | done | `web/p1-19-migration` (517c139…668dfc7) |
| P1-20 | Sign-in, re-auth, device approval, Settings | done | `web/p1-20-auth-ui` (rebased onto P1-02 by the lead; typecheck, lint, vitest and web Playwright 33/33 pass after the rebase) |
| P1-11 | Trash and bulk by selector | done; an independent review is running | `web/p1-11-trash-bulk` (f44eb36, 7cb89a9) |
| P1-23 | First osn PR, part 3: apply stage 2 (DNS, Access, backups) | done on 2026-10-03, except the live SSE measurements (moved to P1-26, see below): `server-v0.1.0-rc.2` deployed; control schema upgraded to v3 and libraries to v2; Hermes unchanged. Backups are on. The first db and media snapshots are saved in `restic/shelfy`, the restore drill verified 2 databases with 0 problems, and `shelfy_backup_last_success` is 1. The Access service token `shelfy-refs-clients` was created by `just cf-apply` after O5 (the user token "Scope Minimo Token" needed Access: Service Tokens Edit) and is stored in SOPS. `/health` answers 200 through Access with it, and 302 without. | osn 44926e5, e65a777 |
| P1-06 | Post modal, folders and Sidebar on the seam | running (Sonnet) | `web/p1-06-modal-folders` |
| P1-08 | Gallery performance and the JS budget | running (Sonnet) | `web/p1-08-gallery-perf` |
| P1-16 | First osn PR, part 1: prepare (code only) | done; draft [osn PR #29](https://github.com/niccolofanton/osn/pull/29) | `web/p1-16-osn-prep` + osn `shelfy/p1-16-prepare` (967c83a…b5b94af) |
| P1-22 | O2: R2 bucket and scoped token for backups | done | owner action O2 (osn `1ce0abe`) |

### Follow-ups

Work that a review or a later finding added to an integrated task.

| # | What | From | Status | Branch |
|---|---|---|---|---|
| F1 | T10 hardening: links redeem only by POST with the token in the URL fragment, CSRF check on every unsafe request, trusted-proxy client IPs, no re-caching of revoked sessions, deny-by-default authentication, mail and cookie fixes | the T10 security review (no critical or high finding) | done | `web/t10-hardening` (b8503e0…0d66c7b) |
| F3 | The scheduler spends a try on `user_locked`: a lock longer than about 2–3 minutes fails that user's queued jobs (3 tries, 30 s then 60 s backoff). A locked user's tries should not count, or should wait for the unlock. | P1-12 report | done in F4 (M2): a locked user's jobs are requeued in 60 s without using a try | `web/f4-backup-fixes` (b0a73a7) |
| F4 | Fix the P1-12 review findings. **High:** `restore-db` can be written to or corrupted through handles that reopen a locked library (H1); a backup job killed by a signal records success (H2). **Medium:** concurrent opens run a migration twice (M1); locked users' jobs fail during a restore (M2, = F3); a migration install writes into a locked library (M3); a full restore accepts a snapshot without a user's library (M4); rollback writes are never reconciled (M5, latent: record and document only). **Low:** L1–L7. | independent review of P1-12 | done | `web/f4-backup-fixes` (6c68126…ed5eb34) |
| F5 | Passkey row ids can be reused after the newest passkey is deleted (`INTEGER PRIMARY KEY` without AUTOINCREMENT), which makes audit ids ambiguous. It needs a control-schema change. | P1-13 report | done in P1-17: control schema v3 rebuilds `passkeys` with AUTOINCREMENT, above every id used before | `web/p1-17-account` (47bbab0) |
| F6 | Fix the P1-03 review findings L1–L6: a write racing an explicit eviction leaves a stale 304 and stale cached stats (L1); a request dropped mid-write commits but never announces (L2); an orphan generation cell can come back after a restore (L3); an identical PATCH still bumps every ETag (L4); manual AI edits keep the old provider (L5); the text caps can exceed the body limit (L6). F6 also fixes P1-10's property test "merging a batch twice equals merging it once", which fails when one key appears twice in a batch with `overwrite_ai`. Review M1 (a misspelled selector filter field selects the whole library) went to P1-05, which owns `FilterParams`. | independent review of P1-03 | done | `web/f6-writes-fixes` (c970498…f823b12) |
| F7 | Latent race in `useDownloadPrefs` (desktop): the hook writes localStorage inside its state updater, so a second toggle can be lost when the hook is moved. P1-20 kept the hook in `Settings`, which avoids the race for now. | P1-20 report | running (Sonnet), in one lane with F2 | `web/f2-f7-desktop-fixes` |
| F8 | `GET /media/{file}`'s duration histogram has no `variant` label, so the §6.2 "rendition p95 ≤ 5 ms" budget cannot isolate `g480` from metrics. | P1-26 tooling | todo | |
| F9 | `POST /me/tokens` mints tokens with no server-side TTL (`ttl: None`). Offer an expiry, and check §2.11 for the default. | P1-26 tooling | todo | |
| F10 | On `/device`, clicking Approve while a re-auth is required spends the sign-in limit: the owner got 429 "Too many attempts" after a few clicks on 2026-10-03. Disable Approve until the re-auth completes, and do not count `reauth_required` answers against the limit. | owner, P1-25 | todo | |
| F2 | Fix the 7 desktop e2e failures that predate the port: 6 in "Downloads – job list" (the spec expects `download-job` rows; the view now groups jobs per post) and 1 in "Browser – URL bar shows Twitter bookmarks URL after switching tab". CI does not run e2e, so nothing caught them. | T12 (reproduced on `7ba4ea2`) | running (Sonnet), in one lane with F7 | `web/f2-f7-desktop-fixes` |

### Carry-over notes

Facts from integrated lanes that a later task must act on. The lead copies each one into that task's brief.

| For | Note | From |
|---|---|---|
| P1-19 | `--merge` inserts new keys with `repo::posts::insert` and merges existing keys with `ingest::duplicates::merge_duplicate`, the "keep the row with archived files" policy. The plain ingest `upsert` never adds files to an existing post, as on the desktop. T9's `survivor_rank` and note/tag folding can switch to the core helpers. | P1-10 |
| P3 | "Unanalyzed" means exactly `ai_status IS NULL`, so `pending` and `error` block AI writes at ingest. Revisit this if P3 marks new posts `pending` at ingest. | P1-10 |
| P2 | Capture ingest feeds sanitizer output into `ingest::merge::upsert_batch` inside `UserDb::write`. Response mapping: inserted→inserted, changed→updated, merged→known. A merge that changes nothing writes nothing. | P1-10 |
| P1-18 | The GHCR package starts private: load the image with `docker save`/`docker load` (G4), or have the owner make the package public. The `SHELFY_VERSION` tag carries a leading `v`. | P1-09 |
| P1-11 | Reuse `PostSelector::resolve`, the core `Selector::sql()` for the UPDATEs, and `library::write(state, user, ChangeReason::Delete, …)`, which returns a `library::Change { value, keys }` and announces from the committing task (F6 L2). `announce_as` is gone. A `{keys}` selection (≤ 500, the inline bulk limit) reaches trashed posts; a filter reaches them only with `trash: true`. "Delete with posts" adds a `withPosts` variant to `CollectionDeleteMode`, using the existing core `DeleteMode::TrashPosts`; until then `mode=withPosts` answers 400. | P1-03 |
| P1-14 | The count pill is `GET /posts/count` with the list's parameters. Select-all is `{filter, exceptKeys}`. | P1-03 |
| P1-17 | Add `(POST, "/api/v1/posts/lookup", Scope::Lookup, true)` to `TOKEN_ROUTES`, plus the bearer security entry. The handler already takes `CurrentUser`. | P1-03 |
| P1-05 | A new list filter must also go into `FilterParams`, or the parity test fails. `includeTotal` can use the count cache in `crates/server/src/library.rs`. P1-03 added `search::index::verify` to your module. | P1-03 |
| P1-06 | `PATCH /posts/{key}` returns the full post, which suits optimistic updates. Refresh folders on `stats.changed`; folder-only writes send `posts.changed` with `keys: []`. The manual AI edit's model is `manual`, where the desktop wrote `manuale`. | P1-03 |
| P1-17 | Token creation and `POST /auth/device/approve` take the `RecentAuth` extractor, which answers 403 `reauth_required` without a recent proof. Reuse the audit actions `session.reauth {method}`, `passkey.*` and `magic_link.create {via, purpose}`. In `GET /me`, the `passkeys` capability comes from `state.auth().passkeys().is_enabled()`. Also do F5. | P1-13 |
| P1-20 | Login page: a passkey button, then on `reauth_required` a dialog offering passkey, email (if `emailLink`) or the CLI link. New SPA route `/login/reauth#<token>` posts `{method:"link", token}`. Settings: list, add (within 5 minutes of a sign-in) and delete passkeys. Options and credentials use the WebAuthn L3 JSON names, so the SPA can use `parse…OptionsFromJSON()` and `credential.toJSON()`. Playwright's virtual authenticator needs resident keys and user verification. | P1-13 |
| P1-24 | Steps: sign in once with `admin login-link`, register a passkey within 5 minutes, sign out, then sign in without a username on macOS Safari and Chrome, on iOS as a home-screen app, on Android, and on a desktop through the phone's QR flow. It must run on `refs.niccolofanton.dev`: the RP ID is the host, and moving hosts orphans every passkey. | P1-13 |
| P1-18 | The first Docker build since L7 compiles the vendored OpenSSL, which needs perl and make; `rust:1.99.0-bookworm` has both. A `Permissions-Policy` header, if any lane adds one, must allow `publickey-credentials-create` and `publickey-credentials-get` for `self`. | P1-13 |
| P1-19 | Map the desktop's manual AI model `manuale` (provider `desktop-local`) to `manual`, and clear the provider, schema version and error, as a web manual edit does (F6 L5). T9 copies `ai_model` verbatim. | P1-03 review, L5 |
| P1-11 | Run destructive bulk operations only after P1-05's strict selector filters have landed (review M1). | P1-03 review |
| P1-18 | Follow the apply plan in the P1-16 report, summarized in `doc/RUNBOOK-shelfy.md` in osn: secrets with `just shelfy-secrets` (generated, never printed; the owner email is asked with echo off); the image through `docker save | ssh … docker load` while the GHCR package is private; a Hermes baseline before and after; `just apply "--check --diff"`, then `just apply "--tags monitoring,shelfy,app"` with backups off; then the checks, `create-owner` and `login-link`. Run `just shelfy-sync` after F4 lands, because the vendored backup scripts predate F4. The `edge` subnet is pinned to `10.91.0.0/24`, which is also `SHELFY_TRUSTED_PROXIES`. | P1-16 |
| P1-23 | `shelfy_backups_enabled: true`, then `just apply "--tags shelfy"`, `just shelfy-backup-now` and `just shelfy-restore-drill`. `just cf-apply` for the G2 service token needs `Access: Service Tokens = Edit` on the OpenTofu Cloudflare token (owner action O5). | P1-16 |
| P1-02 | For the bottom nav, use `useNavigation()` or App's `setView`, which maps a view to its route on the web; "Search" has no route. Add your viewports to `web/playwright.config.ts`. `web/e2e/api.ts` provides an automatic API-mock fixture. | P1-04 |
| P1-06 | The gallery's own modal is not on the route yet: a card click calls `navigate({ name: 'post', key })`, closing calls `back(...)`, and prev/next replace the route. Then merge or drop App's `/p/:key` modal (the `routePost` block), so that one modal owns the route. Sidebar folder clicks already push `/c/:id`. | P1-04 |
| P1-20 | Build the client with the capabilities from `GET /me`, once per session; the client owns the SSE stream. Replace `DevicePage` in `web/src/Root.tsx`, and read the Settings section from the route. `reauth_required` is the hook for the re-auth dialog. After a passkey sign-in, set the session back to "checking" to return to `?next=`. | P1-04 |
| P1-21 | Replace the route mocks with `compose.test` plus `admin synth`. Run the desktop e2e with `CI=1`, or turn off `reuseExistingServer`: otherwise Playwright silently reuses any dev server on port 5173, such as the owner's `pnpm dev`, and tests the wrong checkout. | P1-04 |
| P1-06 | The web HTTP client should honour `Retry-After` on 429. `getPostsByIds` sends one GET per post, so more than 60 ids exceed the per-user burst (20/s, burst 60): switch it to `POST /posts/batch-get`. | P1-15 |
| P1-18 | Job series appear only once a job kind is registered, so until P1-11, P1-17 and P1-19 the job panels have no data. Check that the "Queue stuck" alert does not fire on no-data. | P1-15 |
| P1-26 | Route p95s come from `shelfy_http_request_duration_seconds`, with bounds at 5, 15, 40, 60 and 100 ms. For `g480`, use `histogram_quantile` over `increase(shelfy_rendition_bytes_bucket{variant="g480"}[install window])`. `admin bench` runs with `RateLimitConfig::disabled()`. An authenticated k6 run as one user is held to 20 req/s. | P1-15 |
| P3 | Add the AI-suggest limit (1/s) to `rate_limit::route_scope`. | P1-15 |
| P1-19 | **`shelfy-migrate login`:**<br>1. Call `POST /auth/device/start` without cookie or CSRF headers, and show `userCode` with `verificationUri`.<br>2. Poll every `interval`. On `slow_down`, adopt the new interval; on 429, wait `Retry-After`; on 400 `invalid_device_code`, start over.<br>3. On `approved`, write `token` to the token file with mode 0600.<br><br>Keep the `usage::enqueue` call that `migrations/install.rs` makes when the install moves onto the `migrate` job. | P1-17 |
| P1-20 | **`/device` page:** read the code from the URL fragment. On `reauth_required`, open the re-auth dialog and retry. Tell users to approve only a code their own terminal shows.<br>**Capabilities and consent:** take them from `GET /me`. The consent gate posts to `POST /me/consent`.<br>**Storage:** read `/me/usage`, and refetch on `job.updated` for `usage.recompute`.<br>**Sessions and tokens UIs:** use `/me/sessions` and `/me/tokens`. Creating a token needs a re-auth, and the value is shown once. | P1-17 |
| P1-11 | Call `jobs::usage::enqueue` after a purge. | P1-17 |
| P2 | Mint pairing tokens with `auth::api_tokens::mint` (kind `extension`, a new `Via`). Add the extension routes to `TOKEN_ROUTES`, set `extension: true` in `GET /me`, and use the `TokenUser<scopes::…>` extractors. | P1-17 |
| P4 | With `overwrite_ai` (desktop JSON import), dedupe keys within a batch before calling `upsert_batch`: the desktop `bulkUpsert` is not idempotent on a key that repeats in one overwriting batch, and the port keeps desktop parity (F6). | F6 |
| P1-08 | Synthetic libraries: `admin synth --email … --posts 6000\|20000` (deterministic for a seed; run it with the server stopped). Posts carry no remote URLs, and about a third have no stored cover. | P1-05 |
| P1-11 | The strict selector (review M1) has landed. Bulk trash must drop each post's rows from both search indexes (`index::remove_post` does both), and restore must reindex both. `index::verify` checks both. | P1-05 |
| P1-14 | The count pill and `includeTotal` share one cache entry. There is no facet-values endpoint for category, contentType or aiStatus; assume it waits for P3. | P1-05 |
| P1-26 | `just shelfy-admin bench --user <owner id> [--requests 1000] [--strict]` reads only and prints aggregates, with rate limits off; run it at a quiet time. The first request after the deploy pays the library v2 migration (the trigram infix index), about 0.4 s for 6k posts. | P1-05 |
| P5 | Opening a library whose schema is newer than the build writes `meta['schema.older_build']` (F4 M5), but nothing reads it yet. After a rollback across library v2, run `search::index::rebuild_infix` by hand. Wire the re-derivation before the first migration that adds derived data. A restore now keeps a full backup-API copy of the old library, so it needs about one extra library of free disk. | F4 |
| P1-25 | Build or download `shelfy-migrate` from the same release as the deployed server: the server installs only bundles at its own library schema, which is now v2. Sequence: quit the desktop app; `login <url> --header @access.headers` (a 0600 file with the Access service-token headers from P1-23); `plan --redact`; `run --work-dir …`. If interrupted, re-run the same command. Uploads run at about 10 objects/s under the per-user limit, so about 9 minutes for the reference library; the install took 150 s locally. Expected reconciliation: see the P1-19 report in this file's history. Delete the headers file and the token afterwards. | P1-19 |
| P2 | 220 Instagram covers that are still valid will expire before the archive drain exists. The archive drain and `refresh_media` are P2 (OI-7). | P1-19 |
| P5 | With many users migrating, uploads may need their own rate budget, or tus creation-with-upload so that each object takes one request. | P1-19 |
| P1-06 | Under 900 px, the post modal stacks through CSS in `src/index.css`. It is keyed on the `.postmodal-media-row` class and the `post-modal-meta` testid of `MetaColumn`, so keep both or update the CSS. `Sidebar.tsx` is now wrapped in a drawer: it has its own `drawerOpen` state and the testids `sidebar-open`, `sidebar-backdrop` and `sidebar-close`. Keep that wrapper when you edit folders. | P1-02 |
| P1-08 | `PostCard.tsx` mounts one `window` listener per card for the tap-to-preview broadcast, plus pointer handlers. Count them in the per-card cost. | P1-02 |
| P1-24 | The manifest and icons are served at `/manifest.webmanifest` and `/icons/*`, and `apple-mobile-web-app-capable` is set. Check on the live host that the CSP does not block the manifest or the icons. | P1-02 |
| P1-14 | Long-press already calls `onQuickSelect`, as the hover checkbox does, so wiring `onQuickSelect` in Gallery on the web enables it with no `PostCard.tsx` change. | P1-02 |
| P1-24 | The owner, on each device:<br>1. Sign in with `login-link`; the consent gate appears once per account.<br>2. Go to Settings → Account → Add a passkey → Create. Within 5 minutes of signing in, no confirmation is asked.<br>3. Sign out, then choose "Sign in with a passkey".<br><br>Notes:<br>- **Safari:** the WebAuthn JSON helpers exist from Safari/iOS 18.4, and older versions use the base64url fallback. Record the OS versions tested.<br>- **iOS home-screen app:** it has its own cookies, so register the passkey in Safari first, then sign in inside the app. Record how Access behaves there.<br>- **Desktop:** sign in through the phone's QR flow. | P1-20 |
| P1-21 | The real-server suite is in `web/e2e/server/`. It needs the release binary and the `sqlite3` CLI, configured through `SHELFY_E2E_*`. An old sign-in is simulated by editing `sessions.reauth_at` before the server first reads the session, which it caches for 60 s. Traces are off, because they would keep link tokens. | P1-20 |
| P1-26 | Also measure P1-23's SSE checks on the live host: write → SSE p95 ≤ 300 ms over ≥ 50 events, one stream ≥ 5 min with an idle gap over 100 s, a lossless `Last-Event-ID` resume, and no challenge for bearer calls with the extension, Shortcut and CLI User-Agents. `scripts/spikes/sse-probe.mjs` targets the SPIKE-10 test server (`/api/v1/emit`), so first adapt it to the real API: writes through `POST /collections`, and a short-lived session that the lead mints and revokes after the probe (lane rule 8). `SPIKE_HEADERS` carries the service token. | P1-23 |
| P1-21 | The CSP blocks the inline `<style>` in `src/views/Browser.tsx`, a desktop-only view. | P1-09 |
| P1-14 | P1-11's API: `POST /posts/bulk {selector, action, params?}` answers 200 `BulkResult` (≤ 500 posts) or 202 with a job. `GET /trash` lists newest delete first with `total` and `retentionDays: 30`. `POST /trash/restore` takes `{selector}` (a filter must say `trash: true`) or `{deletedAt}`; undo is restore by the `deletedAt` a delete returned, also for job deletes and `withPosts`. `POST /trash/empty` answers 202. Select-all in the trash is `{filter: {trash: true}, exceptKeys}`. Hide `analyze`, `fetchMedia` and `removeStoredMedia`, which answer 422 `not_available`. `CollectionDeleted` gained `deletedAt`, so mocks need it. | P1-11 |
| P4 | The purge marks objects `unreferenced_since`; the GC (P4-12) deletes files and rows. To add `removeStoredMedia`, add a variant to `shelfy_core::bulk::Action` and drop its `not_available` arm in `BulkAction::resolve`. File-touching actions must always run as jobs; `start()` picks inline or job by count only. | P1-11 |
| P2, P3 | `fetchMedia` (P2) and `analyze` (P3) plug into P1-11's bulk the same way; the job payload `{action, params, selection, at}` is generic. `analyze` may instead delegate to `POST /ai/analyze` and its confirm step. | P1-11 |
| P2 | On Instagram the replay is required for every listing: the passive walker reads nothing from today's saved-folder GraphQL (`PolarisProfilePostsTabContentQuery_connection`). | SPIKE-3 |

### P2–P6

On 2026-10-03, with P1 at 20 of 27 tasks, the owner asked for maximum parallelism. P2, P3 and P4 are broken down at once and will run concurrently with the rest of P1; P5 and P6 are broken down later.

| Lane | What | Status | Branch |
|---|---|---|---|
| P2 plan | Break P2 down into tasks: [phases/P2.md](phases/P2.md), 19 lane tasks in 5 waves | done | `web/p2-plan` |
| P3 plan | Break P3 down into tasks: `phases/P3.md` | running (Opus) | `web/p3-plan` |
| P4 plan | Break P4 down into tasks: [phases/P4.md](phases/P4.md), 28 lane tasks in 5 waves (P4-01 folded into P2-04, L11) | done | `web/p4-plan` |
| SPIKE-9 | On-demand video and link hydration from the VPS (E1): IG video URL lifetime, anonymous routes and yt-dlp, hydration endpoints | running (Opus) | `web/spike9-video` |
| SPIKE-4, SPIKE-11 | Chromium sandbox and Smokescreen egress with the SSRF probes, then the capture v2 cost in a 1.5 GiB / 1.5 CPU container, on the VPS (E1) | running (Opus) | `web/spike4-11-capture` |
| P1-26 prep | Live-check tooling: session and token helpers, the SSE probe on the real API, the §6.2 budget queries | done: `scripts/live/` and its runbook | `web/p1-26-tooling` (75ee458) |

**P2 and P4 lanes.** Task cards are in the phase files; this table is the status.

| Task | What | Status | Branch |
|---|---|---|---|
| P2-02 | Ingest sanitizer and archive-state rule in the core | running (Opus) | `web/p2-02-ingest-core` |
| P2-03 | Pairing, extension config, kill switches, extension status | running (Opus) | `web/p2-03-pairing` |
| P2-04 | Outbound HTTP client, CDN fetcher, host limits, breaker (with P4-01, L11) | running (Opus) | `web/p2-04-outbound` |
| P2-05 | Parser: direct video URLs and an IG REST entry | running (Opus) | `web/p2-05-parser-video` |
| P2-06 | Extension core: build, pairing, API client, offline queue, passive capture | running (Opus) | `web/p2-06-extension-core` |
| P2-07 | PWA, Android share target, `/share` page, bookmarklet | running (Sonnet) | `web/p2-07-pwa-share` |
| P4-04 | Core: web captures, versions, delete modes | running (Opus) | `web/p4-04-web-captures` |
| P4-06 | Media: yt-dlp and ffmpeg tools | running (Opus) | `web/p4-06-video-tools` |
| P4-07 | Quotas, usage accounting, limits | running (Opus) | `web/p4-07-quotas` |
| P4-08 | Web tus uploads: sessions, `uploads` scope, purposes | running (Opus) | `web/p4-08-uploads` |
| P4-09 | Jobs view (replaces Downloads on the web) | running (Sonnet) | `web/p4-09-jobs-view` |

Held back: P4-02 and P4-03 wait for the SPIKE-4/11 note; P2-08 waits for P4-09 (L14); P4-05, P4-10, P4-11 and P4-12 start as slots free up.

## Spike outcomes

| Spike | Result | Consequence |
|---|---|---|
| SPIKE-1 ([note](spikes/01-legacy-mapping.md)) | PASS: 19,976 rows accounted for, 121/121 columns mapped or dropped on purpose, 0 duplicate groups, and every IG shortcode decodes to its pk. 264 video paths point to missing files, and 220 IG covers were still valid. | Open items OI-1…OI-12 in the note go to T9 and P1-19. A dangling video path means the video was not kept; it is not an error. Archive the still-valid IG covers right after install. |
| SPIKE-5 ([note](spikes/05-fts-relevance.md)) | PASS on the frozen baseline pair: nDCG@10 0.653 and MRR 0.861, against a desktop baseline of 0.620 and 0.861. On today's snapshot, nDCG@10 is 0.752 against 0.791, short by 0.019. The one losing case is a query that only matches inside a compound hashtag. Latency p95 is 3.3 ms on 6.1k posts and 20 ms on 18.4k. | Decide on a trigram infix index in P1-05: it reaches desktop parity on both libraries for about 6 MB of index. Keep the frozen pair as the gate until then. Re-tune the column weights in P3, once posts have AI fields. |
| SPIKE-2 ([note](spikes/02-cdn-from-datacenter.md)) | From the VPS: Instagram 280/280 and X 300/300 of the URLs that work from a residential IP; no blocks or throttling. No Pinterest sample yet. | Archive mode: Instagram `server`, X `server`, Pinterest `auto` until O1 provides a sample. Start at 2 req/s per host and raise only while the breaker stays quiet. A 403 for an expired signature is not a breaker signal: check `oe` first and turn expired URLs into extension refresh tasks. Archive right after ingest: 73 % of the library's IG URLs had already expired. |
| SPIKE-3 ([note](spikes/03-extension-capture.md)), partial | IG folder of 123 posts: 123/123 matched with the replay (100 %, PASS). The passive capture alone found 0/123: the folder page's one in-scope GraphQL query (`PolarisProfilePostsTabContentQuery_connection`) gave no items. X bookmarks: 20 items parsed from the first `Bookmarks` page with 0 rejected; there is no desktop baseline yet. Pinterest and background tabs were not run. | On IG the replay is required, as on the desktop: the P2 sync controller runs it for every IG listing. Extending the passive walker to the current GraphQL shape is optional (P2). X and Pinterest parity wait for the rest of O1. |
| SPIKE-10 ([note](spikes/10-sse-tunnel.md)) | Through nginx: SSE p95 8 ms and a lossless `Last-Event-ID` resume. Through Cloudflare: a stream with no heartbeat is cut at about 125 s; the 20 s heartbeat keeps it open. Bearer calls were never challenged, and 16 MiB uploads arrive intact. Quick tunnels buffer SSE, so latency through Cloudflare is still unmeasured. | Measure live SSE latency, a long stream and a resume on the real hostname in P1-23. Bot Fight Mode must be off before P2 removes Access (O4). |

## Owner actions

| # | Action | Needed by | Status |
|---|---|---|---|
| O1 | Load the unpacked extension and run the SPIKE-3 comparison on your own accounts: steps in [spikes/03-extension-capture.md](spikes/03-extension-capture.md) §5 | P2 | partial on 2026-10-02: the IG folder is done. Still to do: X bookmarks to the end with a desktop X import, and a Pinterest board if the owner uses Pinterest. IG saved (all posts) is skipped, because it uses the same replay as the folder over about 4,000 posts. |
| O2 | Appendix B prerequisite: R2 bucket `osn-backups` with a scoped token for restic. DNS Edit is no longer needed: OpenTofu manages DNS records with the existing DNS-scoped token (osn `ebd2de8`) | backups (P1-23) | done on 2026-10-02: the lead created the bucket (WEUR); the owner created the token (Object Read & Write, `osn-backups` only) and stored it with `just r2-backup-secrets`; `just r2-backup-check` passes (osn `1ce0abe`) |
| O3 | Optional: fix Homebrew permissions so local tools such as mailpit can be installed | T10 | not needed: T10 writes sign-in emails to a dev mailbox (`SHELFY_DEV_MAILBOX`) |
| O5 | Add `Access: Service Tokens = Edit` to the Cloudflare API token that OpenTofu uses for Access (a user token, My Profile → API Tokens). Without it, P1-23 cannot create the service token for non-browser clients (G2). | P1-23 | done on 2026-10-02: the token can list Access service tokens |
| O4 | Confirm Bot Fight Mode is off for `niccolofanton.dev` (Security → Bots). Our tokens cannot read zone settings, and with it on, the extension, Shortcut and CLI calls could be challenged | before P2 removes Access | done: the owner confirmed it is off (2026-10-02) |
