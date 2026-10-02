// Starts the throwaway shelfy-server of the web e2e suite on a real server
// (./playwright.config.ts): a fresh data directory with the owner account, then
// `serve` in the foreground until Playwright stops it. The sign-in limit
// counts per client address: the suite's browsers send their own
// `CF-Connecting-IP`, which this server believes from the local proxy.
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { E2E } from './env';

mkdirSync(E2E.dataDir, { recursive: true });
const env = { ...process.env, ...E2E.serverEnv };
execFileSync(E2E.serverBin, ['admin', 'create-owner', '--email', E2E.ownerEmail], {
  env,
  stdio: ['ignore', 'ignore', 'inherit'],
});

const server = spawn(E2E.serverBin, ['serve'], { env, stdio: 'inherit' });
const stop = (): void => {
  if (server.exitCode === null) server.kill('SIGTERM');
};
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
process.on('exit', stop);
server.on('exit', (code, signal) => process.exit(code ?? (signal ? 1 : 0)));
