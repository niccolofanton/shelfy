// Where the web e2e suite on a real server runs, from the environment
// (./playwright.config.ts sets SHELFY_E2E_DATA_DIR once per run, before the
// servers and the workers start, so they all see the same directory).
//
// | Variable                | Default                         | Meaning                    |
// |-------------------------|---------------------------------|----------------------------|
// | `SHELFY_E2E_WEB_PORT`   | 18200                           | the web app (vite preview) |
// | `SHELFY_E2E_API_PORT`   | 18201                           | shelfy-server              |
// | `SHELFY_E2E_DATA_ROOT`  | the system temp directory       | where run directories go   |
// | `SHELFY_E2E_DATA_DIR`   | `<root>/shelfy-e2e-<time>`      | this run's data directory  |
// | `SHELFY_E2E_SERVER_BIN` | `target/release/shelfy-server`  | the server binary          |
// | `SHELFY_E2E_SHOTS`      | none                            | where to save screenshots  |
//
// Passkeys need the RP ID `localhost`: the public URL is the web app's origin
// on localhost, never 127.0.0.1.
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../..', import.meta.url));
const webPort = Number(process.env.SHELFY_E2E_WEB_PORT || 18200);
const apiPort = Number(process.env.SHELFY_E2E_API_PORT || 18201);
const metricsPort = apiPort + 90;
const dataDir =
  process.env.SHELFY_E2E_DATA_DIR ||
  join(process.env.SHELFY_E2E_DATA_ROOT || tmpdir(), `shelfy-e2e-${Date.now()}`);
const origin = `http://localhost:${webPort}`;

export const E2E = {
  repoRoot,
  webPort,
  apiPort,
  origin,
  apiUrl: `http://127.0.0.1:${apiPort}`,
  dataDir,
  controlDb: join(dataDir, 'control', 'control.sqlite'),
  serverBin: resolve(repoRoot, process.env.SHELFY_E2E_SERVER_BIN || 'target/release/shelfy-server'),
  shots: process.env.SHELFY_E2E_SHOTS || null,
  ownerEmail: 'owner@example.test',
  // The server's environment: email goes to a dev mailbox, so the
  // re-authentication dialog offers it too.
  serverEnv: {
    SHELFY_DATA_DIR: dataDir,
    SHELFY_LISTEN_ADDR: `127.0.0.1:${apiPort}`,
    SHELFY_METRICS_ADDR: `127.0.0.1:${metricsPort}`,
    SHELFY_PUBLIC_URL: origin,
    SHELFY_LOG_FORMAT: 'text',
    SHELFY_DEV_MAILBOX: 'true',
    SHELFY_TRUSTED_PROXIES: '127.0.0.1/32,::1/128',
    RUST_LOG: 'warn',
  },
};
