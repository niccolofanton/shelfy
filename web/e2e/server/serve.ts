// Starts the throwaway shelfy-server of the web e2e suite on a real server
// (./playwright.config.ts): a fresh data directory with the owner account,
// plus two more test accounts (P1-21), then `serve` in the foreground until
// Playwright stops it. The sign-in limit counts per client address: the
// suite's browsers send their own `CF-Connecting-IP`, which this server
// believes from the local proxy.
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { E2E } from './env';
import { seedTags } from './seedTags';
import { startAiStub } from './aiStub';

mkdirSync(E2E.dataDir, { recursive: true });
const env = { ...process.env, ...E2E.serverEnv };
execFileSync(E2E.serverBin, ['admin', 'create-owner', '--email', E2E.ownerEmail], {
  env,
  stdio: ['ignore', 'ignore', 'inherit'],
});
// Two more non-owner accounts (`admin create-user`, the same tool E6's mock
// account uses), so that account.spec.ts's and jobs.spec.ts's own "a fresh
// account" tests don't contaminate each other (see env.ts's comment on
// `jobsEmail`) and sse-latency.spec.ts gets posts to work with. `synth` fills
// an empty library and must run while the server is stopped
// (crates/server/src/admin/synth.rs): both calls below happen before `serve`.
for (const email of [E2E.synthEmail, E2E.jobsEmail]) {
  execFileSync(E2E.serverBin, ['admin', 'create-user', '--email', email], {
    env,
    stdio: ['ignore', 'ignore', 'inherit'],
  });
}
execFileSync(
  E2E.serverBin,
  [
    'admin',
    'synth',
    '--email',
    E2E.synthEmail,
    '--posts',
    String(E2E.synthPosts),
    '--profile',
    'reference',
  ],
  { env, stdio: ['ignore', 'ignore', 'inherit'] },
);

seedTags();

const stopStub = await startAiStub();
const server = spawn(E2E.serverBin, ['serve'], { env, stdio: 'inherit' });
const stop = (): void => {
  stopStub();
  if (server.exitCode === null) server.kill('SIGTERM');
};
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
process.on('exit', stop);
server.on('exit', (code, signal) => process.exit(code ?? (signal ? 1 : 0)));
