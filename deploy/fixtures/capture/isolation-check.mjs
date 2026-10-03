// Run inside the production capture image. Never print the internal token.
import assert from 'node:assert/strict';
import process from 'node:process';
import console from 'node:console';
import { URL } from 'node:url';
import { promises as fs } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import { Resolver } from 'node:dns/promises';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import path from 'node:path';

const proxy = process.env.CAPTURE_PROXY;
assert(proxy, 'CAPTURE_PROXY is required');
assert.equal(process.getuid(), 10100, 'capture must run as uid 10100');
const routes = JSON.parse(execFileSync('ip', ['-j', 'route'], { encoding: 'utf8' }));
assert(!routes.some((r) => r.dst === 'default' || r.gateway), 'capture has an outbound gateway');
const v6Routes = JSON.parse(execFileSync('ip', ['-6', '-j', 'route'], { encoding: 'utf8' }));
assert(!v6Routes.some((r) => r.dst === 'default' || r.gateway), 'capture has an IPv6 gateway');
const status = await fs.readFile('/proc/self/status', 'utf8');
assert.match(status, /^NoNewPrivs:\s+1$/m);
assert.match(status, /^CapEff:\s+0+$/m);
assert.match(status, /^Seccomp:\s+2$/m);
try {
  await fs.writeFile('/app/.isolation-write-probe', 'unexpected');
  assert.fail('capture root is writable');
} catch (error) {
  assert(['EROFS', 'EACCES'].includes(error.code), 'root write probe failed unexpectedly');
}
const mounts = await fs.readFile('/proc/mounts', 'utf8');
assert(
  mounts.split('\n').some((line) => {
    const fields = line.split(' ');
    return fields[1] === '/' && fields[3].split(',').includes('ro');
  }),
  'root mount is not read-only',
);
assert.match(mounts, /\S+ \/tmp tmpfs /);
assert.match(mounts, /\S+ \/dev\/shm tmpfs /);

function request(url, options = {}, viaProxy = false) {
  const destination = new URL(url);
  const endpoint = viaProxy ? new URL(proxy) : destination;
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        hostname: endpoint.hostname,
        port: Number(endpoint.port || 80),
        path: viaProxy ? url : destination.pathname + destination.search,
        ...options,
        headers: { host: destination.host, ...options.headers },
      },
      (res) => {
        let body = '';
        res.on('data', (chunk) => {
          body += chunk;
          if (body.length > 1024 * 1024) req.destroy(new Error('probe response too large'));
        });
        res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body }));
        res.on('error', reject);
      },
    );
    req.setTimeout(180_000, () => req.destroy(new Error('probe deadline')));
    req.on('error', reject);
    req.end(options.body);
  });
}
function directBlocked(host, port) {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    socket.setTimeout(1500);
    socket.on('connect', () => {
      socket.destroy();
      reject(new Error('direct egress connected'));
    });
    socket.on('error', () => resolve());
    socket.on('timeout', () => {
      socket.destroy();
      resolve();
    });
  });
}
await Promise.all(
  [
    ['169.254.169.254', 80],
    ['100.64.0.1', 80],
    ['1.1.1.1', 443],
    ['8.8.8.8', 53],
    ['10.134.0.1', 80],
    ['10.134.0.1', 22],
    ['10.133.0.10', 80],
    ...String(process.env.CAPTURE_SELF_IPS || '')
      .split(',')
      .filter(Boolean)
      .map((ip) => [ip, 443]),
  ].map(([host, port]) => directBlocked(host, port)),
);
const resolver = new Resolver({ timeout: 1000, tries: 1 });
resolver.setServers(['127.0.0.11']);
await assert.rejects(resolver.resolve4('example.com'), 'external DNS was answered');
resolver.setServers(['8.8.8.8']);
await assert.rejects(resolver.resolve4('example.com'), 'direct external DNS was answered');

for (const [port, route] of [
  [8080, '/health'],
  [9464, '/metrics'],
]) {
  const result = await request(
    `http://${process.env.CAPTURE_API_IP || '10.134.0.2'}:${port}${route}`,
    {
      headers: { 'cf-connecting-ip': '8.8.8.8', authorization: 'Bearer synthetic-token' },
    },
  );
  assert.equal(result.status, 403, 'API must refuse the capture socket peer');
}
const fixture =
  process.env.CAPTURE_CHECK_FIXTURE || 'http://allowed.fixture.test/capture/probe.html';
assert.equal((await request(fixture, {}, true)).status, 200, 'proxy positive control failed');
for (const route of ['to-private-name', 'to-private-literal', 'to-metadata-literal']) {
  const redirect = await request(`http://redirect.fixture.test/${route}`, {}, true);
  assert.equal(redirect.status, 302);
  assert.equal(
    (await request(redirect.headers.location, {}, true)).status,
    407,
    'proxy accepted a private redirect',
  );
}
// Reuse the P4-02 catalogue, including raw numeric encodings, IPv6, names,
// forbidden ports and proxy chaining. A skipped Chromium probe is not a pass.
execFileSync(
  process.execPath,
  [
    '/app/ssrf-probe.mjs',
    'probe',
    '--proxy',
    proxy,
    '--only',
    'B,C,D,E,G,K,L',
    '--proxy-ips',
    '10.134.0.3',
    '--gateways',
    '10.134.0.1',
  ],
  { stdio: 'inherit', timeout: 300_000 },
);
const sandboxOutput = execFileSync(
  process.execPath,
  ['/app/ssrf-probe.mjs', 'sandbox', '--proxy', proxy],
  { encoding: 'utf8', timeout: 30_000 },
);
const sandbox = JSON.parse(sandboxOutput.trim().split('\n').at(-1));
assert.equal(sandbox.launched, true, 'Chromium did not launch');
if (process.env.SHELFY_DISABLE_SANDBOX !== '1') {
  const renderer = sandbox.processes.find((p) => p.type === 'renderer');
  assert(
    renderer?.count > 0 &&
      renderer.ownUserNs > 0 &&
      renderer.ownPidNs > 0 &&
      renderer.ownNetNs > 0 &&
      renderer.extraSeccomp > 0,
    'renderer lacks sandbox namespaces or its seccomp filter',
  );
}
console.log(
  JSON.stringify({ check: 'sandbox', mode: sandbox.sandboxEnv, processes: sandbox.processes }),
);

const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const id = '0' + [...randomBytes(25)].map((b) => alphabet[b & 31]).join('');
const workDir = path.join('/work', id);
await fs.mkdir(workDir, { mode: 0o750 });
try {
  const response = await request('http://127.0.0.1:8080/v1/captures', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-shelfy-internal-token': process.env.SHELFY_INTERNAL_TOKEN || '',
    },
    body: JSON.stringify({
      captureId: id,
      workDir,
      url: fixture,
      maxPages: 1,
      singlePage: true,
      video: false,
    }),
  });
  assert.equal(response.status, 200);
  const lines = response.body
    .trim()
    .split('\n')
    .map((line) => JSON.parse(line));
  assert(
    lines.length <= 400 && !lines.some((line) => line.type === 'failed'),
    'fixture capture failed',
  );
  assert.equal(lines.at(-1)?.type, 'done');
  const manifest = JSON.parse(await fs.readFile(path.join(workDir, 'manifest.json'), 'utf8'));
  assert.equal(manifest.schema, 2);
  assert.equal(manifest.pages.length, 1);
  const assets = manifest.pages.flatMap((page) => page.assets);
  assert(assets.length > 0, 'capture produced no images');
  for (const asset of assets) {
    assert.match(asset.file, /^[a-z0-9-]{1,64}\.(webp|png|jpg|mp4)$/);
    const stat = await fs.lstat(path.join(workDir, asset.file));
    assert(stat.isFile() && stat.nlink === 1 && stat.size > 0);
  }
  console.log(
    JSON.stringify({
      check: 'fixture-capture',
      pages: manifest.pages.length,
      assets: assets.length,
      bytes: manifest.bytes,
    }),
  );
} finally {
  await fs.rm(workDir, { recursive: true, force: true });
}
console.log('capture isolation checks passed');
