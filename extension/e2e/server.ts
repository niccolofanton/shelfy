// Real local server only; never fall back to an API mock or a user's data.
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { randomBytes } from 'node:crypto';
import { createServer, type Server } from 'node:https';
import { loadNodeSqlite } from '../scripts/compare-lib';
export const PORT = Number(process.env.SHELFY_EXTENSION_E2E_PORT || 18319);
export const ORIGIN = `http://localhost:${PORT}`;
export const CDN_HOST = 'scontent-synth1-1.cdninstagram.com';
export const PNG = readFileSync(resolve('web/public/icons/favicon-32.png'));
export class RealServer {
  readonly bin = resolve(
    process.env.SHELFY_EXTENSION_E2E_SERVER_BIN || 'target/release/shelfy-server',
  );
  readonly data: string;
  env: NodeJS.ProcessEnv;
  private server?: ChildProcess;
  private preview?: ChildProcess;
  private cdn?: Server;
  blockCdn = false;
  cdnRequests = 0;
  cdnRefusals = 0;
  logs: string[] = [];
  constructor(readonly work: string) {
    if (!existsSync(this.bin))
      throw new Error(
        `real shelfy-server missing: set SHELFY_EXTENSION_E2E_SERVER_BIN (${this.bin})`,
      );
    this.data = join(work, 'data');
    mkdirSync(this.data, { recursive: true });
    const cleanEnv = { ...process.env };
    for (const name of Object.keys(cleanEnv)) if (name.startsWith('SHELFY_')) delete cleanEnv[name];
    this.env = {
      ...cleanEnv,
      SHELFY_DATA_DIR: this.data,
      SHELFY_MASTER_KEY: randomBytes(32).toString('base64'),
      SHELFY_LISTEN_ADDR: `127.0.0.1:${PORT + 1}`,
      SHELFY_METRICS_ADDR: `127.0.0.1:${PORT + 2}`,
      SHELFY_PUBLIC_URL: ORIGIN,
      SHELFY_DEV_MAILBOX: 'true',
      SHELFY_TRUSTED_PROXIES: '127.0.0.1/32,::1/128',
      RUST_LOG: 'warn',
      SHELFY_ARCHIVE_RATE_INSTAGRAM: '100',
      SHELFY_ARCHIVE_RATE_X: '100',
      SHELFY_ARCHIVE_RATE_PINTEREST: '100',
    };
  }
  admin(...args: string[]) {
    return execFileSync(this.bin, ['admin', ...args], {
      env: this.env,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  }
  async start() {
    const key = join(this.work, 'cdn.key'),
      cert = join(this.work, 'cdn.pem');
    execFileSync(
      'openssl',
      [
        'req',
        '-x509',
        '-newkey',
        'rsa:2048',
        '-nodes',
        '-days',
        '1',
        '-subj',
        `/CN=${CDN_HOST}`,
        '-addext',
        `subjectAltName=DNS:${CDN_HOST},DNS:pbs.twimg.com,DNS:i.pinimg.com`,
        '-addext',
        'basicConstraints=critical,CA:FALSE',
        '-addext',
        'extendedKeyUsage=serverAuth',
        '-keyout',
        key,
        '-out',
        cert,
      ],
      { stdio: 'ignore' },
    );
    this.cdn = createServer({ key: readFileSync(key), cert: readFileSync(cert) }, (_req, res) => {
      this.cdnRequests++;
      if (this.blockCdn) {
        this.cdnRefusals++;
        res.writeHead(429);
        res.end('fixture CDN refused server');
      } else {
        res.writeHead(200, { 'Content-Type': 'image/png' });
        res.end(PNG);
      }
    });
    this.cdn.on('tlsClientError', (error) => this.logs.push(`fixture TLS: ${error.message}`));
    await new Promise<void>((resolve, reject) => {
      this.cdn!.once('error', reject);
      this.cdn!.listen(PORT + 3, '127.0.0.1', resolve);
    });
    this.env.SHELFY_DEV_EGRESS_HOSTS = [CDN_HOST, 'pbs.twimg.com', 'i.pinimg.com']
      .map((host) => `${host}=127.0.0.1:${PORT + 3}`)
      .join(',');
    this.env.SHELFY_DEV_EGRESS_CA = cert;
    this.admin('create-owner', '--email', 'extension-ci@example.test');
    this.admin('flags', 'set', 'extension.instagram.stopAfterKnown', '2');
    this.admin('flags', 'set', 'extension.instagram.scroll', 'false');
    await this.restart();
    this.preview = spawn(
      'pnpm',
      [
        'exec',
        'vite',
        'preview',
        '--config',
        'web/vite.config.ts',
        '--host',
        'localhost',
        '--port',
        String(PORT),
      ],
      {
        env: { ...process.env, SHELFY_API_URL: `http://127.0.0.1:${PORT + 1}` },
        detached: process.platform !== 'win32',
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    );
    this.preview.stdout?.on('data', () => {});
    this.preview.stderr?.on('data', () => {});
    await ready(ORIGIN);
  }
  async restart() {
    await this.stopServer();
    this.server = spawn(this.bin, ['serve'], { env: this.env, stdio: ['ignore', 'pipe', 'pipe'] });
    const logs = this.logs;
    this.server.stderr?.on('data', (value) => logs.push(String(value)));
    this.server.stdout?.on('data', (value) => logs.push(String(value)));
    try {
      await ready(`http://127.0.0.1:${PORT + 1}/health`);
    } catch (error) {
      writeFileSync(join(this.work, 'server-errors.log'), logs.join(''));
      throw error;
    }
  }
  async stopServer() {
    const child = this.server;
    this.server = undefined;
    if (!child || child.exitCode !== null) return;
    const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
    child.kill('SIGTERM');
    const timer = setTimeout(() => {
      if (child.exitCode === null) child.kill('SIGKILL');
    }, 5000);
    await exited;
    clearTimeout(timer);
  }
  rows(accountId: string, sql: string, ...params: (string | number)[]) {
    if (!/^[A-Za-z0-9_-]+$/.test(accountId)) throw new Error('invalid fixture account');
    const db = new (loadNodeSqlite().DatabaseSync)(
      join(this.data, 'users', accountId, 'library.sqlite'),
      { readOnly: true },
    );
    try {
      return db.prepare(sql).all(...params) as Record<string, unknown>[];
    } finally {
      db.close();
    }
  }
  controlRows(sql: string) {
    const db = new (loadNodeSqlite().DatabaseSync)(join(this.data, 'control', 'control.sqlite'), {
      readOnly: true,
    });
    try {
      return db.prepare(sql).all();
    } finally {
      db.close();
    }
  }
  async close() {
    await this.stopServer();
    if (this.preview?.pid && this.preview.exitCode === null) {
      if (process.platform !== 'win32') process.kill(-this.preview.pid, 'SIGTERM');
      else this.preview.kill('SIGTERM');
    }
    if (this.cdn) {
      this.cdn.closeAllConnections();
      await new Promise<void>((resolve) => this.cdn!.close(() => resolve()));
    }
  }
}
async function ready(url: string) {
  for (let attempt = 0; attempt < 200; attempt++) {
    try {
      const res = await fetch(url);
      if (res.status < 500) return;
    } catch {
      /* startup */
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`fixture listener did not start: ${url}`);
}
