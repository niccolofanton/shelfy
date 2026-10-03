// Dedicated owner library: ordinary account/jobs fixtures remain empty.
// Only loopback shelfy-ai-stub serves model calls, using synthetic tags.
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { E2E } from './env';
import { startAiStub } from './aiStub';
mkdirSync(E2E.dataDir, { recursive: true });
const env = { ...process.env, ...E2E.serverEnv, SHELFY_OPERATOR_AI_EMBED_MODEL: '' };
for (const args of [
  ['admin', 'create-owner', '--email', E2E.ownerEmail],
  [
    'admin',
    'synth',
    '--email',
    E2E.ownerEmail,
    '--posts',
    '24',
    '--profile',
    'reference',
    '--ai-share',
    '1',
  ],
])
  execFileSync(E2E.serverBin, args, { env, stdio: ['ignore', 'ignore', 'inherit'] });
const user = execFileSync(
  'sqlite3',
  [E2E.controlDb, `SELECT id FROM users WHERE email='${E2E.ownerEmail}'`],
  { encoding: 'utf8' },
).trim();
if (!/^[A-Za-z0-9-]+$/.test(user)) throw new Error('Synthetic taxonomy owner missing');
const library = join(E2E.dataDir, 'users', user, 'library.sqlite');
execFileSync('sqlite3', [
  library,
  `
  DELETE FROM post_tags;
  WITH numbered AS (SELECT id, ((row_number() OVER (ORDER BY id)-1)/6) AS g FROM posts)
  INSERT INTO post_tags(post_id,tag_norm,tag_form,source,tier)
  SELECT id,'fixture theme '||g||' tag '||n,'fixture theme '||g||' tag '||n,'ai','specific'
  FROM numbered CROSS JOIN (SELECT 0 AS n UNION ALL SELECT 1 UNION ALL SELECT 2);
  UPDATE posts SET ai_tags_json=(SELECT json_group_array(tag_form) FROM post_tags WHERE post_id=posts.id);
`,
]);
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
