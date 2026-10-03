// Starts the throwaway shelfy-server of the web e2e suite on a real server
// (./playwright.config.ts): a fresh data directory with the owner account,
// plus two more test accounts (P1-21), then `serve` in the foreground until
// Playwright stops it. The sign-in limit counts per client address: the
// suite's browsers send their own `CF-Connecting-IP`, which this server
// believes from the local proxy.
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
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
for (const email of [E2E.synthEmail, E2E.jobsEmail, E2E.queueEmail]) {
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

// A separate synthetic catalog library keeps queue tests independent from
// the empty owner and the reference-media library used by the other suites.
execFileSync(E2E.serverBin, ['admin', 'synth', '--email', E2E.queueEmail, '--posts', '20'], {
  env,
  stdio: ['ignore', 'ignore', 'inherit'],
});
const queueUser = execFileSync(
  'sqlite3',
  [E2E.controlDb, `SELECT id FROM users WHERE email='${E2E.queueEmail}';`],
  { encoding: 'utf8' },
).trim();
if (!/^[A-Za-z0-9_-]+$/.test(queueUser)) throw new Error('Queue fixture user missing');
// The managed operator is owner-only; grant this isolated synthetic fixture
// that role without changing the ordinary owner's empty account.
execFileSync('sqlite3', [
  E2E.controlDb,
  `UPDATE users SET role='owner', quota_bytes=0 WHERE id='${queueUser}';`,
]);
execFileSync(
  'sqlite3',
  [
    join(E2E.dataDir, 'users', queueUser, 'library.sqlite'),
    `UPDATE posts SET platform='twitter', media_type='text', caption='Synthetic catalog lamp', archive_state='done', ai_status=NULL, ai_description=NULL, ai_tags_json=NULL, ai_save_reason=NULL;`,
  ],
  { stdio: ['ignore', 'ignore', 'inherit'] },
);
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
