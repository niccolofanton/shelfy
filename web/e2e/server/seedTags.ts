// Private synthetic Tag Explorer account, initialized before the server opens
// its library. Review proposals never require a live model or real user data.
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { E2E } from './env';
export const TAGS_EMAIL = 'tag-explorer@example.test';
export function seedTags(): void {
  const env = { ...process.env, ...E2E.serverEnv };
  for (const args of [
    ['admin', 'create-user', '--email', TAGS_EMAIL],
    [
      'admin',
      'synth',
      '--email',
      TAGS_EMAIL,
      '--posts',
      '240',
      '--profile',
      'reference',
      '--ai-share',
      '0.7',
    ],
  ])
    execFileSync(E2E.serverBin, args, { env, stdio: ['ignore', 'ignore', 'inherit'] });
  const user = execFileSync(
    'sqlite3',
    [E2E.controlDb, `SELECT id FROM users WHERE email='${TAGS_EMAIL}'`],
    { encoding: 'utf8' },
  ).trim();
  if (!/^[A-Za-z0-9-]+$/.test(user)) throw new Error('Synthetic tag user missing');
  execFileSync('sqlite3', [
    join(E2E.dataDir, 'users', user, 'library.sqlite'),
    `
    INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status,created_at)
    VALUES('fixture-alias','fixture-canonical','fixture-canonical','proposed',0);
    INSERT INTO tag_cluster(id,label,label_norm,status,run_id,created_at,updated_at)
    VALUES(99,'Fixture cluster','fixture cluster','proposed',1,0,0);
    INSERT INTO tag_cluster_membership(tag_norm,cluster_id) VALUES('fixture-lamp',99),('fixture-lamps',99);
  `,
  ]);
}
