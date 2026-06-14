import { describe, it, expect, beforeEach, afterAll } from 'vitest';
import { createRequire } from 'module';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawnSync } from 'child_process';

// archive-utils.js is CJS and dependency-free (no electron), so it can be
// exercised against the real filesystem and the real `tar` binary. The module
// caches the feed URL at module level: reload it fresh where caching matters.
const req = createRequire(import.meta.url);
const MOD = req.resolve('../../electron/archive-utils.js');

function loadFresh() {
  delete req.cache[MOD];
  return req(MOD);
}

let tmpRoot;
const origResourcesPath = process.resourcesPath;

beforeEach(() => {
  tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'archive-utils-test-'));
});

afterAll(() => {
  process.resourcesPath = origResourcesPath;
});

function makeTar(name, args, cwd) {
  const archive = path.join(tmpRoot, name);
  const r = spawnSync('tar', ['-cf', archive, ...args], { cwd, encoding: 'utf8' });
  expect(r.status).toBe(0);
  return archive;
}

describe('extractArchive', () => {
  it('estrae un archivio valido in modo asincrono', async () => {
    const src = path.join(tmpRoot, 'src');
    fs.mkdirSync(src);
    fs.writeFileSync(path.join(src, 'hello.txt'), 'ciao');
    const archive = makeTar('ok.tar', ['hello.txt'], src);

    const { extractArchive } = loadFresh();
    const dest = path.join(tmpRoot, 'dest');
    const p = extractArchive(archive, dest);
    expect(p).toBeInstanceOf(Promise);
    await p;
    expect(fs.readFileSync(path.join(dest, 'hello.txt'), 'utf8')).toBe('ciao');
  });

  it("rifiuta un archivio con membri '..' (zip-slip) senza estrarre nulla", async () => {
    const base = path.join(tmpRoot, 'base');
    const inner = path.join(base, 'inner');
    fs.mkdirSync(inner, { recursive: true });
    fs.writeFileSync(path.join(base, 'evil.txt'), 'evil');
    // -P preserva il membro '../evil.txt' nel listing dell'archivio.
    const archive = makeTar('evil.tar', ['-P', '../evil.txt'], inner);

    const { extractArchive } = loadFresh();
    const dest = path.join(tmpRoot, 'slip-dest');
    await expect(extractArchive(archive, dest)).rejects.toThrow(/zip-slip/);
    expect(fs.existsSync(path.join(tmpRoot, 'evil.txt'))).toBe(false);
  });

  it('rifiuta un archivio inesistente con un errore di listing', async () => {
    const { extractArchive } = loadFresh();
    await expect(
      extractArchive(path.join(tmpRoot, 'missing.tar'), path.join(tmpRoot, 'dest')),
    ).rejects.toThrow(/archive listing failed/);
  });
});

describe('assertSafeArchive', () => {
  it('accetta un archivio con soli percorsi relativi sicuri', async () => {
    const src = path.join(tmpRoot, 'src');
    fs.mkdirSync(path.join(src, 'sub'), { recursive: true });
    fs.writeFileSync(path.join(src, 'sub', 'a.txt'), 'a');
    const archive = makeTar('safe.tar', ['sub'], src);

    const { assertSafeArchive } = loadFresh();
    await expect(assertSafeArchive(archive)).resolves.toBeUndefined();
  });
});

describe('readFeedUrl', () => {
  function writeYml(url) {
    fs.writeFileSync(path.join(tmpRoot, 'app-update.yml'), `provider: generic\nurl: ${url}\n`);
  }

  it('legge la URL HTTPS dal file e la cachea al primo successo', () => {
    process.resourcesPath = tmpRoot;
    writeYml('https://feed.example.com/shelfy/');
    const { readFeedUrl } = loadFresh();
    expect(readFeedUrl()).toBe('https://feed.example.com/shelfy');
    // Il file cambia ma il valore resta quello cacheato (una sola lettura).
    writeYml('https://other.example.com');
    expect(readFeedUrl()).toBe('https://feed.example.com/shelfy');
    // Un modulo fresco rilegge dal disco.
    expect(loadFresh().readFeedUrl()).toBe('https://other.example.com');
  });

  it('rifiuta una URL non HTTPS', () => {
    process.resourcesPath = tmpRoot;
    writeYml('http://insecure.example.com');
    const { readFeedUrl } = loadFresh();
    expect(readFeedUrl()).toBeNull();
  });

  it('torna null se il file manca, senza cacheare il fallimento', () => {
    process.resourcesPath = path.join(tmpRoot, 'nope');
    const { readFeedUrl } = loadFresh();
    expect(readFeedUrl()).toBeNull();
    process.resourcesPath = tmpRoot;
    writeYml('https://feed.example.com');
    expect(readFeedUrl()).toBe('https://feed.example.com');
  });
});
