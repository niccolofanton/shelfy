// Where the web e2e suite on a real server runs, from the environment
// (./playwright.config.ts sets SHELFY_E2E_DATA_DIR once per run, before the
// servers and the workers start, so they all see the same directory).
//
// | Variable                 | Default                         | Meaning                    |
// |---------------------------|---------------------------------|----------------------------|
// | `SHELFY_E2E_WEB_PORT`     | 18200                           | the web app (vite preview) |
// | `SHELFY_E2E_API_PORT`     | 18201                           | shelfy-server              |
// | `SHELFY_E2E_DATA_ROOT`    | the system temp directory       | where run directories go   |
// | `SHELFY_E2E_DATA_DIR`     | `<root>/shelfy-e2e-<time>`      | this run's data directory  |
// | `SHELFY_E2E_SERVER_BIN`   | `target/release/shelfy-server`  | the server binary          |
// | `SHELFY_E2E_SYNTH_POSTS`  | 200                             | the owner's synth library  |
// | `SHELFY_E2E_SHOTS`        | none                            | where to save screenshots  |
//
// Passkeys need the RP ID `localhost`: the public URL is the web app's origin
// on localhost, never 127.0.0.1. The session cookie (`__Host-shelfy_session`)
// needs the same: it is `Secure`, and Playwright's `request` clients (unlike
// a real page's own fetch/EventSource, which Chromium already special-cases)
// only attach a `Secure` cookie to a request whose URL says `https:` or
// `localhost` — never a plain `http://127.0.0.1` one, even on the loopback
// (found by P1-21's sse-latency.spec.ts: every `page.request` call 401ed).
import { tmpdir } from 'node:os';
import { randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../..', import.meta.url));
const webPort = Number(process.env.SHELFY_E2E_WEB_PORT || 18200);
const apiPort = Number(process.env.SHELFY_E2E_API_PORT || 18201);
const metricsPort = apiPort + 90;
const stubPort = apiPort + 1;
const stubControlPort = apiPort + 2;
const byokStubPort = apiPort + 3;
// Generate once in the runner; its child servers and workers inherit the values.
process.env.SHELFY_E2E_MASTER_KEY ||= randomBytes(32).toString('base64');
process.env.SHELFY_E2E_STUB_KEY ||= randomBytes(32).toString('base64');
const dataDir =
  process.env.SHELFY_E2E_DATA_DIR ||
  join(process.env.SHELFY_E2E_DATA_ROOT || tmpdir(), `shelfy-e2e-${Date.now()}`);
const origin = `http://localhost:${webPort}`;

export const E2E = {
  repoRoot,
  webPort,
  apiPort,
  stubPort,
  stubUrl: `http://127.0.0.1:${stubPort}`,
  stubControlUrl: `http://127.0.0.1:${stubControlPort}`,
  stubControlPort,
  byokStubPort,
  byokStubUrl: `http://[::1]:${byokStubPort}`,
  stubLatencyMs: Number(process.env.SHELFY_E2E_STUB_LATENCY_MS || 0),
  stubBin: resolve(repoRoot, process.env.SHELFY_E2E_STUB_BIN || 'target/release/shelfy-ai-stub'),
  origin,
  apiUrl: `http://localhost:${apiPort}`,
  dataDir,
  controlDb: join(dataDir, 'control', 'control.sqlite'),
  serverBin: resolve(repoRoot, process.env.SHELFY_E2E_SERVER_BIN || 'target/release/shelfy-server'),
  shots: process.env.SHELFY_E2E_SHOTS || null,
  // The owner's account stays an empty library: account.spec.ts and
  // jobs.spec.ts both rely on "a fresh account" (no jobs, no posts).
  ownerEmail: 'owner@example.test',
  // P1-21: a second, non-owner member account (`admin create-user`, the same
  // tool E6's mock account uses) with its own small `admin synth` library,
  // for specs that need real posts — today just sse-latency.spec.ts — without
  // disturbing the owner's "fresh account" to account.spec.ts/jobs.spec.ts.
  // `admin synth` fills an empty library and must run with the server
  // stopped, so this happens once in serve.ts, before `serve`. Kept small so
  // every real-server run stays fast; P1-08's own 6k/20k libraries and the
  // Lighthouse job's are unrelated, bigger, standalone data directories.
  synthEmail: 'synth@example.test',
  synthPosts: Number(process.env.SHELFY_E2E_SYNTH_POSTS || 60),
  // P1-21: jobs.spec.ts needs "empty, no jobs queued" on its first sign-in
  // too, so it cannot share the owner — account.spec.ts's Storage test
  // triggers the owner's first `usage.recompute` (crates/server/src/routes/
  // me/usage.rs: "until the first [count] it is null, and this request
  // starts one"), and Playwright always runs spec files in the same sorted
  // order regardless of CLI argument order, so "account" runs before "jobs"
  // every time either file is run as part of the full suite.
  jobsEmail: 'jobs@example.test',
  queueEmail: 'queue@example.test',
  // The server's environment: email goes to a dev mailbox, so the
  // re-authentication dialog offers it too.
  serverEnv: {
    SHELFY_DATA_DIR: dataDir,
    SHELFY_MASTER_KEY: process.env.SHELFY_E2E_MASTER_KEY,
    SHELFY_AI_ALLOW_LOOPBACK: 'true',
    SHELFY_OPERATOR_AI_URL: `http://127.0.0.1:${stubPort}/v1`,
    SHELFY_OPERATOR_AI_KEY: process.env.SHELFY_E2E_STUB_KEY,
    SHELFY_OPERATOR_AI_MODEL: 'stub-text',
    SHELFY_OPERATOR_AI_VISION_MODEL: 'stub-vision',
    SHELFY_OPERATOR_AI_EMBED_MODEL: 'stub-embed',
    SHELFY_OPERATOR_AI_LABEL: 'E2E AI node',
    SHELFY_EGRESS_ALLOW_ORIGINS: `http://127.0.0.1:${stubPort}`,
    SHELFY_LISTEN_ADDR: `127.0.0.1:${apiPort}`,
    SHELFY_METRICS_ADDR: `127.0.0.1:${metricsPort}`,
    SHELFY_PUBLIC_URL: origin,
    SHELFY_LOG_FORMAT: 'text',
    SHELFY_DEV_MAILBOX: 'true',
    SHELFY_TRUSTED_PROXIES: '127.0.0.1/32,::1/128',
    RUST_LOG: 'warn',
  },
};
