# Live-host tooling (P1-26, X2)

Live checks of the [§6.2 performance budgets](../../docs/web-port/IMPLEMENTATION-PLAN.md#62-performance-budgets)
and the [SPIKE-10 SSE leftovers](../../docs/web-port/spikes/10-sse-tunnel.md) on the real host,
`https://refs.niccolofanton.dev`, plus the [mock account](#mock-account-e6)'s seeded subset (E6,
X2). Built for P1-26 prep and X2; run by the lead, ahead of (or as) the cards in
[`docs/web-port/phases/P1.md`](../../docs/web-port/phases/P1.md) and
[`docs/web-port/EXECUTION.md`](../../docs/web-port/EXECUTION.md).

The session and token tools (`session.mjs`, and everything under "Runbook" below) hold a session
and a token only for the duration of one run, minted from a login link the lead creates out of
band and revoked at the end — never the owner's own browser session (P1 lane rule 8, "no owner
session in automation"); the [mock account](#mock-account-e6) is the one exception the rule itself
allows.

Node ESM, no dependencies beyond Node 24's built-ins (`fetch`; `subset-library.mjs` also uses
`node:sqlite`). Every command below assumes:

```sh
export PATH="/Users/fant/.nvm/versions/node/v24.20.0/bin:$PATH"   # the default `node` is v14
```

## Runbook

The live host sits behind Cloudflare Access: every request needs the service-token headers
(`CF-Access-Client-Id`, `CF-Access-Client-Secret`). Put them in a 0600 file, one `Name: value` per
line (the same format `scripts/spikes/sse-probe.mjs` reads from `SPIKE_HEADERS`):

```sh
cat > access.headers <<'EOF'
CF-Access-Client-Id: <id>
CF-Access-Client-Secret: <secret>
EOF
chmod 600 access.headers
```

1. **Mint a login link**, on the VPS, next to the server:

   ```sh
   shelfy-server admin login-link --email "$SHELFY_OWNER_EMAIL" \
     --public-url https://refs.niccolofanton.dev
   ```

   Copy the printed URL (`https://refs.niccolofanton.dev/login/magic#<token>`). It works once and
   expires in 15 minutes.

2. **Redeem it:**

   ```sh
   node scripts/live/session.mjs redeem '<the login-link URL>' \
     --out session.json --headers access.headers
   ```

3. **Mint a token** (within 5 minutes of step 2 — minting needs a "recent" sign-in; redeem again if
   this answers `reauth_required`):

   ```sh
   node scripts/live/session.mjs token --session session.json \
     --out token.json --headers access.headers
   ```

4. **Run the checks:**

   ```sh
   node scripts/live/sse-live.mjs --session session.json --token token.json \
     --headers access.headers --json --out sse-result.json

   node scripts/live/budgets.mjs --vm <tunneled VictoriaMetrics URL> --window 1h \
     > budgets-report.md
   ```

   `sse-live.mjs` takes about 8–9 minutes without `--quick` (a ~110 s latency phase, a ~5 minute
   long-stream phase with a deliberate >100 s idle gap, a resume check, then the bearer-call
   phase). It is safe to Ctrl-C at any point: every probe folder it created so far is deleted
   before it exits.

5. **Revoke:**

   ```sh
   node scripts/live/session.mjs revoke --session session.json --token token.json \
     --headers access.headers
   ```

6. **Delete the files** (`revoke` already deletes `session.json` and `token.json` themselves —
   this step is whatever is left: `access.headers` and the two result files, once their numbers
   are copied into `docs/web-port/reports/p1-vps.md`):

   ```sh
   rm -f access.headers sse-result.json budgets-report.md
   ```

Paste `budgets-report.md` and the `sse-live.mjs` summary into the P1-26 section of
`docs/web-port/reports/p1-vps.md` (owned by the P1-26 card), then do step 6.

### Security notes

- The session and token this tooling mints live at most as long as the run: `redeem` creates them,
  `revoke` (step 5) deletes them server-side, and no later step reuses them.
- `session.json`, `token.json` and `access.headers` are written 0600 (owner read/write only) and
  hold secrets (a session cookie, a bearer token, the Access service-token credentials). None of
  them are committed, and `revoke` deletes the first two itself — delete `access.headers` and any
  `--out` result files yourself (step 6).
- Nothing these tools print is a secret value: `redeem` and `token` log the user id and token id,
  never the cookie or the token string.

## Mock account (E6)

The owner decided (E6, [`docs/web-port/EXECUTION.md`](../../docs/web-port/EXECUTION.md)) that the
live host needs a **mock account**: a regular member, seeded with about 500 of the owner's posts,
that lanes may sign in to in automation — the one exception lane rule 8 ("no owner session in
automation") itself carves out; it is never the owner's own account.

Built for X2, with two new pieces:

- `shelfy-server admin create-user` (the admin CLI, on the VPS, next to the server; not one of this
  directory's scripts — see `crates/server/src/admin/create_user.rs`) creates the member account
  itself. No Node, no Access headers: it runs locally on the server's own data directory, like
  `admin create-owner`.
- [`subset-library.mjs`](subset-library.mjs) (Node, in this directory) builds the seeded subset, as
  a desktop-schema SQLite file `shelfy-migrate` can read, from a copy of the desktop library — never
  from the live server, and it needs no Access headers either.

### The lead's steps, once

1. **Create the mock user, on the VPS** (`docker exec`, or wherever `admin` already runs):

   ```sh
   shelfy-server admin create-user --email <mock email, kept in osn secrets like the owner's> \
     --display-name "Mock account"
   ```

   It is idempotent and prints only the new id (never the email) — note the id down.

2. **Mint a `migrate` token for that user, on the VPS** (valid 7 days, shown once):

   ```sh
   shelfy-server admin migrate-token --email <mock email> > migrate-token.txt
   chmod 600 migrate-token.txt
   ```

3. **Build the subset, locally**, from a copy of the desktop library (never the live server, and
   never the owner's library directly — a snapshot of it, per lane rule 9):

   ```sh
   node scripts/live/subset-library.mjs --db <desktop shelfy.sqlite> --out <a local dir> \
     --count 500 --seed <n> --platform-mix ig:0.6,x:0.35,web:0.05
   ```

   Prints aggregate counts only (lane rule 9) and writes `<dir>/shelfy.sqlite`. See the comment at
   the top of that file for exactly how the sample and the cascades work, and
   `subset-library.fixture.mjs` for the synthetic fixture it is tested against (never the owner's
   library) — `docs/web-port/EXECUTION.md`'s X2 report has the reconciliation from that fixture and
   from one redacted dry run on the owner's reference snapshot.

4. **Check it, locally, then migrate it onto the live host:**

   ```sh
   shelfy-migrate plan --db <dir>/shelfy.sqlite --redact        # 0 unmapped columns, 0 orphan rows
   shelfy-migrate run --db <dir>/shelfy.sqlite --media-root <desktop userData dir> \
     --server https://refs.niccolofanton.dev --token "$(cat migrate-token.txt)" \
     --header @access.headers --work-dir <a scratch dir>
   ```

   `run` reads only the files the subset's rows reference (`crates/migrate/src/files.rs`), not
   every file under `--media-root`, so the smaller `posts` table alone is what uploads less — the
   media store itself is untouched, nothing is deleted from it.

5. **Check:** `shelfy-server admin bench --user <mock id>` on the VPS, or sign in (next section)
   and browse. Delete `migrate-token.txt` once the run succeeds.

### A lane: signing in to the mock account

The lead hands a lane the mock account's email (or user id) out of band. From there, same pattern
as this directory's other tools — a login link minted on the VPS, redeemed locally:

```sh
ssh <vps> "shelfy-server admin login-link --email <mock email> --public-url https://refs.niccolofanton.dev"
# copy the printed URL

node scripts/live/session.mjs redeem '<the login-link URL>' --out session.json --headers access.headers
```

`session.json`'s cookie is the lane's session for its own end-to-end checks against the mock
account (X3: every web feature, against `docs/web-port/features/`) — not run through this
directory's P1-26-specific tools, which are for the owner's own account and the §6.2 budgets. When
the lane's checks are done:

```sh
node scripts/live/session.mjs revoke --session session.json --headers access.headers
rm -f access.headers
```

## What each tool does

- **`session.mjs`** — `redeem`, `token`, `revoke`. See the comment at the top of the file for the
  exact options of each subcommand.
- **`sse-live.mjs`** — the [SPIKE-10](../../docs/web-port/spikes/10-sse-tunnel.md) probe
  (`scripts/spikes/sse-probe.mjs`), adapted from its throwaway test server to the real API:
  - **writes** through `POST`/`DELETE /api/v1/collections`, a folder named `zz-probe-<run>`,
    created and deleted repeatedly; every folder it creates is tracked and deleted again on exit,
    including on error or `SIGINT`/`SIGTERM`;
  - **latency**: write → the matching `posts.changed` event on `GET /api/v1/events`, p50/p95/max
    over ≥ 50 events (budget: p95 ≤ 300 ms). Every write is paced 2.2 s apart — just over the
    server's 2 s `posts.changed` throttle window (`crates/server/src/events/coalesce.rs`) — so a
    coalesced event never gets misread as network latency;
  - **long stream**: one stream held open ≥ 5 minutes, with one idle gap over 100 s (Cloudflare's
    proxied-connection cutoff), counting heartbeats and confirming no disconnect;
  - **resume**: closes the stream, writes N events, reconnects with `Last-Event-ID`, and checks
    all N come back, in order, with no duplicates;
  - **bearer calls**: the API token, with the User-Agents of the extension, the iOS Shortcut and
    the CLI, against `POST /api/v1/posts/lookup` (a bearer-scoped route that takes an API token or
    a session) — no 403, no HTML, no `cf-mitigated`;
  - `--quick` shrinks the latency sample count and the long-stream phase to a local-testing size
    that does **not** reach the real ≥ 5 min / > 100 s budget; the report says so and that one
    sub-check is left out of the pass/fail verdict (everything else is scored normally).
- **`budgets.mjs`** — the §6.2 server budgets from VictoriaMetrics: route p95/p99 from
  `shelfy_http_request_duration_seconds` and the `g480` rendition size from
  `shelfy_rendition_bytes{variant="g480"}`, as a markdown table, plus the budgets these tools
  cannot measure and the command to run each (`admin bench`, a k6 TTFB run, Lighthouse). `--fixture
  <file>` replaces the live VictoriaMetrics query with a canned response, for a dry run or
  CI-less verification — see `scripts/live/budgets.fixture.json` for the exact JSON shape
  (the Prometheus HTTP API's `/api/v1/query` vector format).
- **`subset-library.mjs`** — the mock account's seeded subset (E6), from a copy of a desktop
  library: a random but deterministic sample of `--count` posts (`--seed`, optionally
  `--platform-mix ig:0.6,x:0.35,web:0.05`), with every dependent row (`post_media`,
  `post_collections`, `post_tags`, `post_entities`, `post_facets`, `web_snapshots`, `downloads`)
  dropped or kept the same way the desktop's own `ON DELETE CASCADE` foreign keys would, and
  collections left with no member removed. Needs only Node's built-in `node:sqlite`. Opens `--db`
  read-only and immutable, and never writes next to it — see the comment at the top of the file for
  exactly how a live (open, or not cleanly closed) source is handled safely instead of refused.
  Prints aggregate counts only (lane rule 9).
- **`subset-library.fixture.mjs`** — the synthetic desktop-schema database `subset-library.mjs` is
  tested against (never the owner's library): `node subset-library.fixture.mjs <path>` writes it
  standalone, or `import { buildFixture } from './subset-library.fixture.mjs'` from another script.
  Every string in it is made up.

## Gaps found while building this

- **No per-write correlation id.** `posts.changed`'s payload (`{keys, reason}`) carries nothing
  that ties one event to the write that caused it (unlike `scripts/spikes/sse-probe.mjs`'s
  synthetic `probe` event, which carried `{phase, seq}` for exactly this). `sse-live.mjs`
  correlates by serializing every write and taking the next event in arrival order — correct only
  because this tool is the sole writer on the account for the run's duration (P1 lane rule 8 keeps
  it that way: no concurrent owner session). A request id on `posts.changed`, or the collection id
  it is about, would make this robust to concurrent activity too.
- **`GET /media/{file}`'s route histogram doesn't separate by rendition variant.** The §6.2 "media
  rendition p95 ≤ 5 ms" budget is measured through this route's `shelfy_http_request_duration_seconds`,
  which has no `variant` label (only `shelfy_rendition_bytes`, a *write-time* size histogram, has
  one) — so the measured p95 covers every object this route serves (originals, posters, `g480`
  alike), not the `g480` reads the budget is really about. `budgets.mjs` surfaces this as a note on
  that row rather than silently reporting a number that doesn't mean what the budget says.
- **`POST /me/tokens` mints a token with no expiry** (`ttl: None` is hardcoded in
  `crates/server/src/routes/me/tokens.rs`'s `create_token`; only the device flow's `migrate` token
  and `admin migrate-token` pass a TTL). `session.mjs token`'s "short-lived" is enforced by this
  tooling (`revoke` deletes it) rather than the server; a token this run fails to revoke — a crash
  between `token` and `revoke` that also loses the session needed to call `DELETE /me/tokens/{id}`
  — stays valid until someone revokes it by hand from `GET /me/tokens`.
