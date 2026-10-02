# Shelfy online — execution log

Live status of [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md). The plan is the spec; this file records how the work runs, what is done, and what waits on the owner.

## How the work runs

- **Integration branch:** `web/foundations`, branched from `dev`. Every task lands on it as a fast-forward and is pushed; CI must be green.
- **Lanes:** one task per lane. A lane is a Claude Code subagent in its own git worktree, on branch `web/<task>-<slug>`, started from the tip of `web/foundations`.
- **Lead:** one session plans the waves, reviews each lane (diff, checks, and an independent reviewer for substantive tasks), integrates it and updates this file.
- **Concurrency:** a new lane starts only while 8 GB of local disk stay free. Even with line tables only in dev builds (7ba4ea2), a Rust lane's target dir reaches 3.5–6 GB, 1.3–2.5 GB of it incremental, so lanes started after 2026-10-02 18:10 build with `CARGO_INCREMENTAL=0`.
- **Restarts:** lanes run inside the lead's process. Anything that restarts it, such as a permission-mode change, stops every running lane. Worktrees and transcripts survive: the lead resumes each lane with a message, as on 2026-10-02 at 18:05.

## Lane rules

1. Read the plan sections your task cites before writing code. The plan is the spec.
2. Branch from the integration tip: `git switch -c web/<task>-<slug> web/foundations`.
3. Run `pnpm install --frozen-lockfile` once; it installs the git hooks and lets you run the desktop checks.
4. Rust uses the toolchain pinned in `rust-toolchain.toml`. Run cargo with `CARGO_TARGET_DIR="$PWD/target"`: with a shared target dir, lanes overwrite each other's test binaries.
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
| P1-04 | Client seam: routes, SSE client, error boundary, error codes | running | `web/p1-04-client-seam` |
| P1-10 | Merge rules in the core and golden parity | done | `web/p1-10-merge` (ab6efbd, 2364d64) |
| P1-05 | Search and filters complete, search-eval gate, `admin synth`/`bench` | running | `web/p1-05-search` |
| P1-15 | Metrics, log redaction, rate limits | running | `web/p1-15-observability` |
| P1-17 | Account API, API tokens, device-code flow | running | `web/p1-17-account` |
| P1-16 | First osn PR, part 1: prepare (code only) | running | `web/p1-16-osn-prep` + osn `shelfy/p1-16-prepare` |
| P1-22 | O2: R2 bucket and scoped token for backups | done | owner action O2 (osn `1ce0abe`) |

### Follow-ups

Work that a review or a later finding added to an integrated task.

| # | What | From | Status | Branch |
|---|---|---|---|---|
| F1 | T10 hardening: links redeem only by POST with the token in the URL fragment, CSRF check on every unsafe request, trusted-proxy client IPs, no re-caching of revoked sessions, deny-by-default authentication, mail and cookie fixes | the T10 security review (no critical or high finding) | done | `web/t10-hardening` (b8503e0…0d66c7b) |
| F3 | The scheduler spends a try on `user_locked`: a lock longer than about 2–3 minutes fails that user's queued jobs (3 tries, 30 s then 60 s backoff). A locked user's tries should not count, or should wait for the unlock. | P1-12 report | moved into F4 (review finding M2) | |
| F4 | Fix the P1-12 review findings. **High:** `restore-db` can be written to or corrupted through handles that reopen a locked library (H1); a backup job killed by a signal records success (H2). **Medium:** concurrent opens run a migration twice (M1); locked users' jobs fail during a restore (M2, = F3); a migration install writes into a locked library (M3); a full restore accepts a snapshot without a user's library (M4); rollback writes are never reconciled (M5, latent: record and document only). **Low:** L1–L7. | independent review of P1-12 | running | `web/f4-backup-fixes` |
| F5 | Passkey row ids can be reused after the newest passkey is deleted (`INTEGER PRIMARY KEY` without AUTOINCREMENT), which makes audit ids ambiguous. It needs a control-schema change. | P1-13 report | todo, folded into P1-17, which changes the control schema | |
| F6 | Fix the P1-03 review findings L1–L6: a write racing an explicit eviction leaves a stale 304 and stale cached stats (L1); a request dropped mid-write commits but never announces (L2); an orphan generation cell can come back after a restore (L3); an identical PATCH still bumps every ETag (L4); manual AI edits keep the old provider (L5); the text caps can exceed the body limit (L6). F6 also fixes P1-10's property test "merging a batch twice equals merging it once", which fails when one key appears twice in a batch with `overwrite_ai`. Review M1 (a misspelled selector filter field selects the whole library) went to P1-05, which owns `FilterParams`. | independent review of P1-03 | running | `web/f6-writes-fixes` |
| F2 | Fix the 7 desktop e2e failures that predate the port: 6 in "Downloads – job list" (the spec expects `download-job` rows; the view now groups jobs per post) and 1 in "Browser – URL bar shows Twitter bookmarks URL after switching tab". CI does not run e2e, so nothing caught them. | T12 (reproduced on `7ba4ea2`) | todo | |

### Carry-over notes

Facts from integrated lanes that a later task must act on. The lead copies each one into that task's brief.

| For | Note | From |
|---|---|---|
| P1-19 | `--merge` inserts new keys with `repo::posts::insert` and merges existing keys with `ingest::duplicates::merge_duplicate`, the "keep the row with archived files" policy. The plain ingest `upsert` never adds files to an existing post, as on the desktop. T9's `survivor_rank` and note/tag folding can switch to the core helpers. | P1-10 |
| P3 | "Unanalyzed" means exactly `ai_status IS NULL`, so `pending` and `error` block AI writes at ingest. Revisit this if P3 marks new posts `pending` at ingest. | P1-10 |
| P2 | Capture ingest feeds sanitizer output into `ingest::merge::upsert_batch` inside `UserDb::write`. Response mapping: inserted→inserted, changed→updated, merged→known. A merge that changes nothing writes nothing. | P1-10 |
| P1-18 | The GHCR package starts private: load the image with `docker save`/`docker load` (G4), or have the owner make the package public. The `SHELFY_VERSION` tag carries a leading `v`. | P1-09 |
| P1-11 | Reuse `PostSelector::resolve`, the core `Selector::sql()` for the UPDATEs, and `library::write` with `announce_as(ChangeReason::Delete, …)`. A `{keys}` selection (≤ 500, the inline bulk limit) reaches trashed posts; a filter reaches them only with `trash: true`. "Delete with posts" adds a `withPosts` variant to `CollectionDeleteMode`, using the existing core `DeleteMode::TrashPosts`; until then `mode=withPosts` answers 400. | P1-03 |
| P1-14 | The count pill is `GET /posts/count` with the list's parameters. Select-all is `{filter, exceptKeys}`. | P1-03 |
| P1-17 | Add `(POST, "/api/v1/posts/lookup", Scope::Lookup, true)` to `TOKEN_ROUTES`, plus the bearer security entry. The handler already takes `CurrentUser`. | P1-03 |
| P1-05 | A new list filter must also go into `FilterParams`, or the parity test fails. `includeTotal` can use the count cache in `crates/server/src/library.rs`. P1-03 added `search::index::verify` to your module. | P1-03 |
| P1-06 | `PATCH /posts/{key}` returns the full post, which suits optimistic updates. Refresh folders on `stats.changed`; folder-only writes send `posts.changed` with `keys: []`. The manual AI edit's model is `manual`, where the desktop wrote `manuale`. | P1-03 |
| P1-17 | Token creation and `POST /auth/device/approve` take the `RecentAuth` extractor, which answers 403 `reauth_required` without a recent proof. Reuse the audit actions `session.reauth {method}`, `passkey.*` and `magic_link.create {via, purpose}`. In `GET /me`, the `passkeys` capability comes from `state.auth().passkeys().is_enabled()`. Also do F5. | P1-13 |
| P1-20 | Login page: a passkey button, then on `reauth_required` a dialog offering passkey, email (if `emailLink`) or the CLI link. New SPA route `/login/reauth#<token>` posts `{method:"link", token}`. Settings: list, add (within 5 minutes of a sign-in) and delete passkeys. Options and credentials use the WebAuthn L3 JSON names, so the SPA can use `parse…OptionsFromJSON()` and `credential.toJSON()`. Playwright's virtual authenticator needs resident keys and user verification. | P1-13 |
| P1-24 | Steps: sign in once with `admin login-link`, register a passkey within 5 minutes, sign out, then sign in without a username on macOS Safari and Chrome, on iOS as a home-screen app, on Android, and on a desktop through the phone's QR flow. It must run on `refs.niccolofanton.dev`: the RP ID is the host, and moving hosts orphans every passkey. | P1-13 |
| P1-18 | The first Docker build since L7 compiles the vendored OpenSSL, which needs perl and make; `rust:1.99.0-bookworm` has both. A `Permissions-Policy` header, if any lane adds one, must allow `publickey-credentials-create` and `publickey-credentials-get` for `self`. | P1-13 |
| P1-19 | Map the desktop's manual AI model `manuale` (provider `desktop-local`) to `manual`; T9 copies `ai_model` verbatim. | P1-03 review, L5 |
| P1-11 | Run destructive bulk operations only after P1-05's strict selector filters have landed (review M1). | P1-03 review |
| P1-21 | The CSP blocks the inline `<style>` in `src/views/Browser.tsx`, a desktop-only view. | P1-09 |
| P2 | On Instagram the replay is required for every listing: the passive walker reads nothing from today's saved-folder GraphQL (`PolarisProfilePostsTabContentQuery_connection`). | SPIKE-3 |

### P2–P6

Each phase is broken down into tasks when the previous one is close to done.

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
| O4 | Confirm Bot Fight Mode is off for `niccolofanton.dev` (Security → Bots). Our tokens cannot read zone settings, and with it on, the extension, Shortcut and CLI calls could be challenged | before P2 removes Access | done: the owner confirmed it is off (2026-10-02) |
