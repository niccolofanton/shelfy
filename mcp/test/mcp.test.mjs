import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer as httpServer } from 'node:http';
import { once } from 'node:events';
import { mkdtemp, readFile, writeFile, chmod, symlink, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer } from '../dist/server.js';
import { baseUrl, loadConfig, privateFile, saveConfig } from '../dist/config.js';
import { ShelfyApi } from '../dist/api.js';
const tokens = {
  read: 'shx_read_synthetic_01234567890123456789',
  write: 'shx_write_synthetic_01234567890123456789',
  other: 'shx_other_synthetic_01234567890123456789',
};
const output = (result) => result.structuredContent;
async function fixture(t) {
  const requests = [];
  const server = httpServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : undefined;
    const url = new URL(req.url, 'http://fixture.test');
    requests.push({
      method: req.method,
      path: url.pathname,
      query: url.searchParams,
      body,
      headers: req.headers,
    });
    res.setHeader('content-type', 'application/json');
    const auth = req.headers.authorization;
    const owner = auth === `Bearer ${tokens.other}` ? 'other' : 'owner';
    const read = [tokens.read, tokens.write, tokens.other].some(
      (token) => auth === `Bearer ${token}`,
    );
    const readonly =
      req.method === 'GET' ||
      ['/api/v1/posts/lookup', '/api/v1/posts/batch-get'].includes(url.pathname);
    if (!read) {
      res.writeHead(401);
      return res.end(JSON.stringify({ detail: `private reflection ${auth}` }));
    }
    if (!readonly && auth !== `Bearer ${tokens.write}`) {
      res.writeHead(403);
      return res.end(JSON.stringify({ detail: auth }));
    }
    if (url.pathname === '/api/v1/stats') return res.end(JSON.stringify({ total: 2 }));
    if (url.pathname === '/api/v1/posts/owner-key' && owner === 'other') {
      res.writeHead(404);
      return res.end('{}');
    }
    if (url.pathname === '/api/v1/posts/owner-key')
      return res.end(
        JSON.stringify({ key: 'owner-key', userTags: ['Glass'], userNote: 'synthetic owner note' }),
      );
    if (url.pathname === '/api/v1/posts/slow') return setTimeout(() => res.end('{}'), 1000);
    if (url.pathname === '/api/v1/posts/limited') {
      res.writeHead(429, { 'retry-after': '3' });
      return res.end(JSON.stringify({ detail: auth }));
    }
    if (url.pathname === '/api/v1/posts/redirect') {
      res.writeHead(302, { location: '/leak' });
      return res.end('{}');
    }
    if (url.pathname === '/api/v1/posts/large')
      return res.end(JSON.stringify({ value: 'x'.repeat(2 * 1024 * 1024) }));
    if (url.pathname === '/api/v1/posts') {
      return res.end(
        JSON.stringify(
          url.searchParams.get('cursor')
            ? { items: [{ key: 'p2', aiTags: ['design'], userTags: ['GLASS'] }], nextCursor: null }
            : {
                items: [{ key: 'p1', aiTags: ['glass', 'design'], userTags: ['Glass'] }],
                nextCursor: 'cursor-page2',
              },
        ),
      );
    }
    if (url.pathname === '/api/v1/search')
      return res.end(
        JSON.stringify({ items: [{ key: `${owner}-key` }], nextCursor: null, total: 1 }),
      );
    if (url.pathname === '/api/v1/posts/batch-get')
      return res.end(
        JSON.stringify({
          items: body.keys.includes(`${owner}-key`) ? [{ key: `${owner}-key` }] : [],
        }),
      );
    if (url.pathname === '/api/v1/posts/lookup')
      return res.end(
        JSON.stringify({ items: [{ key: body.keys[0], postKey: `${owner}-key`, trashed: false }] }),
      );
    if (url.pathname === '/api/v1/collections' && req.method === 'GET')
      return res.end(JSON.stringify({ items: [{ id: 1, name: 'Synthetic', count: 2 }] }));
    if (url.pathname === '/api/v1/links')
      return res.end(JSON.stringify({ key: 'saved-key', platform: 'web', created: true }));
    return res.end(JSON.stringify({ ok: true }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => {
    server.closeAllConnections();
    server.close();
  });
  return { url: `http://127.0.0.1:${server.address().port}`, requests };
}
async function connect(t, config) {
  const server = createServer(config);
  const client = new Client({ name: 'synthetic', version: '1' });
  const [a, b] = InMemoryTransport.createLinkedPair();
  await server.connect(a);
  await client.connect(b);
  t.after(async () => {
    await client.close();
    await server.close();
  });
  return client;
}
test('official SDK initialization lists only scoped read tools by default', async (t) => {
  const f = await fixture(t);
  const client = await connect(t, { url: f.url, token: tokens.read, write: false });
  const { tools } = await client.listTools();
  assert.equal(tools.length, 8);
  assert(tools.every((tool) => tool.annotations.readOnlyHint));
  assert(
    !tools.some((tool) =>
      /save|update|delete|create|provider|account|admin|suggest/.test(tool.name),
    ),
  );
  assert.equal(
    output(await client.callTool({ name: 'shelfy_library_stats', arguments: {} })).total,
    2,
  );
  assert.equal(f.requests[0].headers.authorization, `Bearer ${tokens.read}`);
  assert.equal(f.requests[0].headers.cookie, undefined);
});
test('search/filter read tools map exact F21 routes, queries and read-only POSTs', async (t) => {
  const f = await fixture(t);
  const client = await connect(t, { url: f.url, token: tokens.read, write: false });
  await client.callTool({
    name: 'shelfy_search_posts',
    arguments: {
      q: 'lamp & glass',
      tags: ['glass', 'design'],
      tagMode: 'and',
      scope: 'sites',
      limit: 20,
    },
  });
  assert.equal(f.requests[0].path, '/api/v1/search');
  assert.deepEqual(f.requests[0].query.getAll('tags'), ['glass', 'design']);
  assert.equal(f.requests[0].query.get('scope'), 'sites');
  assert.equal(f.requests[0].query.get('q'), 'lamp & glass');
  await client.callTool({
    name: 'shelfy_list_posts',
    arguments: {
      collection: 3,
      mediaType: ['images', 'website'],
      sort: 'oldest',
      includeTotal: true,
    },
  });
  assert.equal(f.requests[1].query.get('collection'), '3');
  assert.deepEqual(f.requests[1].query.getAll('mediaType'), ['images', 'website']);
  assert.equal(
    output(await client.callTool({ name: 'shelfy_get_post', arguments: { key: 'owner-key' } })).key,
    'owner-key',
  );
  assert.equal(
    output(
      await client.callTool({
        name: 'shelfy_get_posts',
        arguments: { keys: ['owner-key', 'other-key'] },
      }),
    ).items.length,
    1,
  );
  assert.equal(
    output(
      await client.callTool({
        name: 'shelfy_lookup_posts',
        arguments: { platform: 'instagram', keys: ['shortcode'] },
      }),
    ).items[0].postKey,
    'owner-key',
  );
  assert.equal(
    output(await client.callTool({ name: 'shelfy_list_folders', arguments: {} })).items[0].id,
    1,
  );
});
test('tag traversal returns real deduplicated scoped tags and honest partial counts', async (t) => {
  const f = await fixture(t);
  const client = await connect(t, { url: f.url, token: tokens.read, write: false });
  const first = output(
    await client.callTool({
      name: 'shelfy_list_tags',
      arguments: { scope: 'sites', collection: 2 },
    }),
  );
  assert.equal(first.complete, false);
  assert.equal(first.scannedPosts, 1);
  assert.equal(first.nextCursor, 'cursor-page2');
  assert.equal(first.tags.find((tag) => tag.tag.toLowerCase() === 'glass').countInScannedPosts, 1);
  const all = output(
    await client.callTool({
      name: 'shelfy_list_tags',
      arguments: { scope: 'sites', collection: 2, maxPages: 2 },
    }),
  );
  assert.equal(all.complete, true);
  assert.equal(all.scannedPosts, 2);
  assert.equal(all.tags.find((tag) => tag.tag.toLowerCase() === 'glass').countInScannedPosts, 2);
  const continued = output(
    await client.callTool({
      name: 'shelfy_list_tags',
      arguments: { cursor: first.nextCursor, tagQuery: 'glass' },
    }),
  );
  assert.equal(continued.complete, false);
  assert.equal(continued.tags.length, 1);
  assert.equal(f.requests[0].query.get('source'), 'web');
  assert.equal(f.requests[0].query.get('collection'), '2');
  assert(f.requests.every((req) => req.path === '/api/v1/posts'));
});
test('write opt-in exposes all useful save/edit/folder tools and exact bodies', async (t) => {
  const f = await fixture(t);
  const client = await connect(t, { url: f.url, token: tokens.write, write: true });
  assert.equal((await client.listTools()).tools.length, 16);
  assert.equal(
    output(
      await client.callTool({
        name: 'shelfy_save_post',
        arguments: { url: 'https://example.com/reference', note: 'synthetic', tags: ['glass'] },
      }),
    ).created,
    true,
  );
  assert.deepEqual(f.requests.at(-1).body, {
    url: 'https://example.com/reference',
    note: 'synthetic',
    tags: ['glass'],
  });
  await client.callTool({
    name: 'shelfy_update_post',
    arguments: { key: 'owner-key', userTags: ['new'], userNote: null },
  });
  assert.deepEqual(f.requests.at(-1).body, { userTags: ['new'], userNote: null });
  await client.callTool({
    name: 'shelfy_create_folder',
    arguments: { name: 'Ideas', color: '#abc' },
  });
  assert.equal(f.requests.at(-1).method, 'POST');
  await client.callTool({
    name: 'shelfy_update_folder',
    arguments: { id: 1, name: 'More', position: 0 },
  });
  assert.deepEqual(f.requests.at(-1).body, { name: 'More', position: 0 });
  await client.callTool({ name: 'shelfy_delete_folder', arguments: { id: 1 } });
  assert.equal(f.requests.at(-1).query.get('mode'), 'label');
  await client.callTool({
    name: 'shelfy_add_to_folder',
    arguments: { id: 1, selector: { keys: ['owner-key'] } },
  });
  assert.deepEqual(f.requests.at(-1).body, { selector: { keys: ['owner-key'] } });
  await client.callTool({
    name: 'shelfy_remove_from_folder',
    arguments: { id: 1, key: 'owner-key' },
  });
  assert.equal(f.requests.at(-1).method, 'DELETE');
  await client.callTool({
    name: 'shelfy_folder_from_selection',
    arguments: { name: 'Selected', selector: { filter: { tag: 'glass' }, exceptKeys: ['x'] } },
  });
  assert.equal(f.requests.at(-1).path, '/api/v1/collections/from-query');
});
test('scope/authz/account errors are typed without reflection or cross-account fallback', async (t) => {
  const f = await fixture(t);
  const read = await connect(t, { url: f.url, token: tokens.read, write: true });
  const denied = await read.callTool({
    name: 'shelfy_save_post',
    arguments: { url: 'https://example.com/' },
  });
  assert.equal(denied.isError, true);
  assert.equal(output(denied).error.code, 'forbidden');
  const invalid = await connect(t, {
    url: f.url,
    token: 'shx_invalid_synthetic_01234567890123456789',
    write: false,
  });
  const unauthorized = await invalid.callTool({ name: 'shelfy_library_stats', arguments: {} });
  assert.equal(output(unauthorized).error.code, 'unauthorized');
  assert(!JSON.stringify(unauthorized).includes('shx_'));
  const other = await connect(t, { url: f.url, token: tokens.other, write: false });
  assert.equal(
    output(await other.callTool({ name: 'shelfy_get_post', arguments: { key: 'owner-key' } })).error
      .code,
    'not_found',
  );
  assert.equal(
    output(await other.callTool({ name: 'shelfy_search_posts', arguments: { q: 'owner note' } }))
      .items[0].key,
    'other-key',
  );
  const limit = await other.callTool({ name: 'shelfy_get_post', arguments: { key: 'limited' } });
  assert.equal(output(limit).error.retryAfter, 3);
  assert(!JSON.stringify(limit).includes(tokens.other));
});
test('strict arguments refuse typos, unsafe selectors/keys, and unknown tool methods', async (t) => {
  const f = await fixture(t);
  const client = await connect(t, { url: f.url, token: tokens.write, write: true });
  for (const argumentsValue of [
    { id: 1, selector: { filter: { colection: 2 } } },
    { id: 1, selector: { filter: {} } },
    { id: 1, selector: { keys: [] } },
  ]) {
    assert.equal(
      (await client.callTool({ name: 'shelfy_add_to_folder', arguments: argumentsValue })).isError,
      true,
    );
  }
  assert.equal(
    (await client.callTool({ name: 'shelfy_get_post', arguments: { key: '..' } })).isError,
    true,
  );
  assert.equal(
    (await client.callTool({ name: 'shelfy_search_posts', arguments: { scope: 'web' } })).isError,
    true,
  );
  const unknown = await client
    .callTool({ name: 'shelfy_generic_request', arguments: { path: '/api/v1/me/providers' } })
    .catch(() => ({ isError: true }));
  assert.equal(unknown.isError, true);
  assert.equal(f.requests.length, 0);
});
test('origin-pinned transport refuses redirects, bounds responses, and cancels requests', async (t) => {
  const f = await fixture(t);
  const api = new ShelfyApi({ url: f.url, token: tokens.read, write: false });
  await assert.rejects(
    api.request('GET', '/api/v1/posts/redirect'),
    (error) => error.code === 'server_unreachable',
  );
  assert(!f.requests.some((req) => req.path === '/leak'));
  await assert.rejects(
    api.request('GET', '/api/v1/posts/large'),
    (error) => error.code === 'response_too_large',
  );
  await assert.rejects(
    api.request('GET', '/api/v1/posts/slow', { signal: AbortSignal.timeout(10) }),
    (error) => error.code === 'request_cancelled_or_timed_out',
  );
  await assert.rejects(
    api.request('POST', '/api/v1/links', { body: { url: 'https://example.com' } }),
    (error) => error.code === 'write_disabled',
  );
  await assert.rejects(
    api.request('GET', 'https://evil.test/api/v1/posts'),
    (error) => error.code === 'invalid_route',
  );
});
test('fixed CF Access headers accompany only the pinned Shelfy request', async (t) => {
  const f = await fixture(t);
  const api = new ShelfyApi({
    url: f.url,
    token: tokens.read,
    write: false,
    access: { clientId: 'synthetic-id', clientSecret: 'synthetic-secret' },
  });
  await api.request('GET', '/api/v1/stats');
  assert.equal(f.requests[0].headers['cf-access-client-id'], 'synthetic-id');
  assert.equal(f.requests[0].headers['cf-access-client-secret'], 'synthetic-secret');
  await assert.rejects(api.request('GET', '/api/v1/posts/redirect'));
  assert.equal(f.requests.length, 2);
});
test('private config stores token atomically, rejects unsafe files/URL sources and never echoes secrets', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'shelfy-mcp-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'config', 'mcp.json');
  await saveConfig(path, { url: 'https://shelfy.example/', token: tokens.read, write: false });
  assert.equal((await stat(path)).mode & 0o077, 0);
  assert.equal((await stat(join(dir, 'config'))).mode & 0o077, 0);
  assert.deepEqual(await loadConfig(path, false, {}), {
    url: 'https://shelfy.example',
    token: tokens.read,
    write: false,
  });
  assert.equal((await loadConfig(path, true, {})).write, true);
  const link = join(dir, 'link');
  await symlink(path, link);
  await assert.rejects(privateFile(link));
  await chmod(path, 0o644);
  await assert.rejects(privateFile(path));
  await chmod(path, 0o600);
  await assert.rejects(loadConfig(path, false, { SHELFY_MCP_TOKEN: tokens.read }));
  await assert.rejects(loadConfig(path, false, { SHELFY_MCP_CF_ACCESS_CLIENT_ID: 'one' }));
  for (const url of [
    'http://remote.example',
    'https://user:secret@example.com',
    'https://example.com/api',
    'file:///tmp/x',
    'https://example.com/?token=x',
  ])
    assert.throws(() => baseUrl(url));
  assert.equal(baseUrl('http://127.0.0.1:1234'), 'http://127.0.0.1:1234');
  await writeFile(
    path,
    JSON.stringify({
      url: 'https://example.com',
      token: tokens.read,
      headers: { authorization: 'secret' },
    }),
    { mode: 0o600 },
  );
  await assert.rejects(
    loadConfig(path, false, {}),
    (error) => !error.message.includes(tokens.read),
  );
});
test('official SDK stdio subprocess initializes, calls real fixture and leaves stdout protocol-only', async (t) => {
  const f = await fixture(t);
  const dir = await mkdtemp(join(tmpdir(), 'shelfy-stdio-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'private', 'mcp.json');
  await saveConfig(path, { url: f.url, token: tokens.read, write: false });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [resolve('dist/cli.js'), '--config', path],
    stderr: 'pipe',
    env: { PATH: process.env.PATH || '' },
  });
  let stderr = '';
  transport.stderr.on('data', (chunk) => {
    stderr += chunk;
  });
  const client = new Client({ name: 'stdio-test', version: '1' });
  t.after(() => client.close());
  await client.connect(transport);
  assert.equal((await client.listTools()).tools.length, 8);
  assert.equal(
    output(await client.callTool({ name: 'shelfy_library_stats', arguments: {} })).total,
    2,
  );
  await client.close();
  assert.equal(stderr, '');
});
test('configuration CLI reads redirected token, emits only generic stderr and rejects token args', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'shelfy-configure-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'private', 'mcp.json');
  const child = spawn(
    process.execPath,
    ['dist/cli.js', 'configure', '--url', 'https://shelfy.example', '--config', path],
    { stdio: ['pipe', 'pipe', 'pipe'], env: { PATH: process.env.PATH || '' } },
  );
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => (stdout += chunk));
  child.stderr.on('data', (chunk) => (stderr += chunk));
  child.stdin.end(tokens.read + '\n');
  const [status] = await once(child, 'exit');
  assert.equal(status, 0);
  assert.equal(stdout, '');
  assert(!stderr.includes(tokens.read));
  assert.equal(JSON.parse(await readFile(path, 'utf8')).token, tokens.read);
  const bad = spawn(process.execPath, ['dist/cli.js', '--token', tokens.read], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let error = '';
  bad.stderr.on('data', (chunk) => (error += chunk));
  assert.equal((await once(bad, 'exit'))[0], 1);
  assert(!error.includes(tokens.read));
});
