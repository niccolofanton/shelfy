#!/usr/bin/env node
// Lighthouse CI on the Gallery (P1-21; plan §6.1, §6.2, §3.8): a real
// shelfy-server, built from source, serving its own web/dist (so the audited
// page carries the production CSP and headers, not a separate `vite preview`
// proxy), seeded by `admin synth` with a 6k-post library — the plan's own
// budget workload ("Gallery LCP, mobile profile … ≤ 2.5 s"). `web/
// lighthouserc.json` holds the static part of the config; this script
// supplies the two things that only exist once the server is up: the origin
// to audit and a one-time sign-in link for `web/e2e/lighthouse/login.cjs`.
//
//   pnpm run web:build
//   CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
//     cargo build --release -p shelfy-server
//   pnpm run lighthouse:gallery
//
// Env (all optional): SHELFY_LHCI_PORT (18202), SHELFY_LHCI_POSTS (6000),
// SHELFY_LHCI_DATA_DIR (a fresh temp dir), SHELFY_E2E_SERVER_BIN
// (target/release/shelfy-server), SHELFY_LHCI_KEEP_DATA (keep the data dir
// for inspection instead of deleting it on exit).
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from '@playwright/test';

const here = fileURLToPath(new URL('.', import.meta.url));
const repoRoot = resolve(here, '../../..');

const PORT = Number(process.env.SHELFY_LHCI_PORT || 18202);
const ORIGIN = `http://127.0.0.1:${PORT}`;
const POSTS = Number(process.env.SHELFY_LHCI_POSTS || 6000);
const DATA_DIR = process.env.SHELFY_LHCI_DATA_DIR || join(tmpdir(), `shelfy-lhci-${Date.now()}`);
const SERVER_BIN = resolve(
  repoRoot,
  process.env.SHELFY_E2E_SERVER_BIN || 'target/release/shelfy-server',
);
const WEB_DIR = resolve(repoRoot, 'web/dist');
const EMAIL = 'lighthouse@example.test';

function log(...args: unknown[]): void {
  console.log('[lighthouse]', ...args);
}

async function waitForHealth(url: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      /* not listening yet */
    }
    if (Date.now() > deadline) throw new Error(`${url} never became healthy`);
    await sleep(200);
  }
}

// Brotli/gzip siblings of web/dist's text files, the same rule the image
// build uses (deploy/docker/shelfy-api.Dockerfile): files over 1 KiB of the
// listed extensions. `pnpm run web:build` alone leaves none — only the
// Docker image build step adds them — and shelfy-server falls back to
// compressing on the fly (crates/server/src/app.rs's CompressionLayer) when
// a sibling is missing. On-the-fly brotli/gzip is CPU work Lighthouse's
// mobile CPU throttle (4x slowdown) feels a lot more than a real client
// behind the edge does, so without this step the LCP budget below would
// fail on compression cost alone, not on anything P1-21 is meant to catch.
// Non-fatal if the tools are missing locally: .github/workflows/ci.yml's
// Lighthouse job installs them in CI (apt-get install brotli; gzip is
// already on the runner).
const COMPRESSIBLE_EXT = new Set([
  '.js',
  '.mjs',
  '.css',
  '.html',
  '.svg',
  '.json',
  '.webmanifest',
  '.txt',
  '.xml',
  '.wasm',
  '.map',
]);
const MIN_COMPRESS_BYTES = 1024;

function listFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFiles(path));
    else out.push(path);
  }
  return out;
}

function precompressWebDist(): void {
  const files = listFiles(WEB_DIR).filter(
    (f) => COMPRESSIBLE_EXT.has(extname(f)) && statSync(f).size > MIN_COMPRESS_BYTES,
  );
  const tools: [string, string[]][] = [
    ['brotli', ['--best', '--keep', '--force']],
    ['gzip', ['--best', '--keep', '--force', '--no-name']],
  ];
  for (const [bin, args] of tools) {
    try {
      for (const file of files) execFileSync(bin, [...args, file], { stdio: 'ignore' });
      log(`${bin}: compressed ${files.length} file(s) of web/dist`);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      log(`skipping ${bin} precompression (${detail}); the on-the-fly compressor will handle it`);
    }
  }
}

// `admin login-link` prints exactly one line starting with the public URL
// (web/e2e/server/support.ts's loginLink() does the same parsing).
function loginLink(env: NodeJS.ProcessEnv): string {
  const out = execFileSync(SERVER_BIN, ['admin', 'login-link', '--email', EMAIL], {
    env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const url = out
    .split('\n')
    .map((line) => line.trim())
    .find((line) => line.startsWith(ORIGIN));
  if (!url) throw new Error('admin login-link printed no link');
  return url;
}

async function main(): Promise<void> {
  if (!existsSync(SERVER_BIN)) {
    throw new Error(
      `no shelfy-server at ${SERVER_BIN}: run \`cargo build --release -p shelfy-server\` first, or set SHELFY_E2E_SERVER_BIN`,
    );
  }
  if (!existsSync(join(WEB_DIR, 'index.html'))) {
    throw new Error(`no ${WEB_DIR}/index.html: run \`pnpm run web:build\` first`);
  }
  precompressWebDist();

  mkdirSync(DATA_DIR, { recursive: true });
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    SHELFY_DATA_DIR: DATA_DIR,
    SHELFY_LISTEN_ADDR: `127.0.0.1:${PORT}`,
    SHELFY_METRICS_ADDR: `127.0.0.1:${PORT + 90}`,
    SHELFY_PUBLIC_URL: ORIGIN,
    SHELFY_WEB_DIR: WEB_DIR,
    SHELFY_LOG_FORMAT: 'text',
    SHELFY_DEV_MAILBOX: 'true',
    SHELFY_TRUSTED_PROXIES: '127.0.0.1/32,::1/128',
    RUST_LOG: 'warn',
  };

  log(`seeding ${POSTS} posts for ${EMAIL} in ${DATA_DIR}`);
  execFileSync(SERVER_BIN, ['admin', 'create-owner', '--email', EMAIL], {
    env,
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  // `synth` fills an empty library only and must run while the server is
  // stopped (crates/server/src/admin/synth.rs).
  execFileSync(
    SERVER_BIN,
    ['admin', 'synth', '--email', EMAIL, '--posts', String(POSTS), '--profile', 'reference'],
    { env, stdio: ['ignore', 'ignore', 'inherit'] },
  );

  log('starting shelfy-server');
  const server = spawn(SERVER_BIN, ['serve'], { env, stdio: 'inherit' });
  let stopped = false;
  const stopServer = (): void => {
    if (!stopped && server.exitCode === null) {
      stopped = true;
      server.kill('SIGTERM');
    }
  };
  process.on('exit', stopServer);
  process.on('SIGINT', () => {
    stopServer();
    process.exit(1);
  });

  try {
    await waitForHealth(`${ORIGIN}/health`, 30_000);
    // Minted against the running server, as the operator runs it in
    // production (crates/server/src/admin/mod.rs: admin commands "run next
    // to the server … on the same data directory").
    const loginUrl = loginLink(env);

    log('server healthy; running lhci autorun');
    const chromePath = chromium.executablePath();
    const lhciBin = resolve(repoRoot, 'node_modules/.bin/lhci');
    const configPath = resolve(repoRoot, 'web/lighthouserc.json');
    execFileSync(
      lhciBin,
      [
        'autorun',
        `--config=${configPath}`,
        `--collect.url=${ORIGIN}/`,
        `--collect.chromePath=${chromePath}`,
        `--collect.puppeteerLaunchOptions.executablePath=${chromePath}`,
      ],
      {
        // `CHROME_PATH` for `autorun`'s own healthcheck phase, which runs
        // before `collect` and does not read the `--collect.*` overrides
        // above: chrome-launcher (its Chrome-detection library) reads this
        // env var, the standard way to point it at a non-system Chrome.
        env: { ...env, SHELFY_LHCI_LOGIN_URL: loginUrl, CHROME_PATH: chromePath },
        stdio: 'inherit',
        cwd: repoRoot,
      },
    );
    log('Lighthouse budgets passed');
  } finally {
    stopServer();
    if (!process.env.SHELFY_LHCI_KEEP_DATA) {
      rmSync(DATA_DIR, { recursive: true, force: true });
    }
  }
}

main().catch((err: unknown) => {
  console.error('[lighthouse]', err);
  process.exitCode = 1;
});
