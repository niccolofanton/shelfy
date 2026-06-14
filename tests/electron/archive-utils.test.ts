import { describe, it, expect, beforeEach, afterAll, vi } from 'vitest';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawnSync } from 'child_process';

// archive-utils.ts is dependency-free (no electron, no db), so it can be
// exercised against the real filesystem and the real `tar` binary. The module
// caches the feed URL at module level: reload it fresh (vi.resetModules +
// dynamic import) where caching matters so each fresh import re-evaluates the
// module and resets the cached value.
interface ArchiveUtils {
  extractArchive: (archive: string, dest: string) => Promise<void>;
  assertSafeArchive: (archive: string) => Promise<void>;
  readFeedUrl: () => string | null;
}

async function loadFresh(): Promise<ArchiveUtils> {
  vi.resetModules();
  return (await import('../../electron/archive-utils')) as unknown as ArchiveUtils;
}

// `process.resourcesPath` is an Electron-injected property typed `readonly` by
// the platform types; the test reads/sets it to point archive-utils at a temp
// resources dir. A narrow writable view keeps that mutation type-safe.
const proc = process as unknown as { resourcesPath: string | undefined };

let tmpRoot: string;
const origResourcesPath = proc.resourcesPath;

beforeEach(() => {
  tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'archive-utils-test-'));
});

afterAll(() => {
  proc.resourcesPath = origResourcesPath;
});

function makeTar(name: string, args: string[], cwd: string): string {
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

    const { extractArchive } = await loadFresh();
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

    const { extractArchive } = await loadFresh();
    const dest = path.join(tmpRoot, 'slip-dest');
    await expect(extractArchive(archive, dest)).rejects.toThrow(/zip-slip/);
    expect(fs.existsSync(path.join(tmpRoot, 'evil.txt'))).toBe(false);
  });

  it('rifiuta un archivio inesistente con un errore di listing', async () => {
    const { extractArchive } = await loadFresh();
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

    const { assertSafeArchive } = await loadFresh();
    await expect(assertSafeArchive(archive)).resolves.toBeUndefined();
  });
});

describe('readFeedUrl', () => {
  function writeYml(url: string): void {
    fs.writeFileSync(path.join(tmpRoot, 'app-update.yml'), `provider: generic\nurl: ${url}\n`);
  }

  it('legge la URL HTTPS dal file e la cachea al primo successo', async () => {
    proc.resourcesPath = tmpRoot;
    writeYml('https://feed.example.com/shelfy/');
    const { readFeedUrl } = await loadFresh();
    expect(readFeedUrl()).toBe('https://feed.example.com/shelfy');
    // Il file cambia ma il valore resta quello cacheato (una sola lettura).
    writeYml('https://other.example.com');
    expect(readFeedUrl()).toBe('https://feed.example.com/shelfy');
    // Un modulo fresco rilegge dal disco.
    expect((await loadFresh()).readFeedUrl()).toBe('https://other.example.com');
  });

  it('rifiuta una URL non HTTPS', async () => {
    proc.resourcesPath = tmpRoot;
    writeYml('http://insecure.example.com');
    const { readFeedUrl } = await loadFresh();
    expect(readFeedUrl()).toBeNull();
  });

  it('torna null se il file manca, senza cacheare il fallimento', async () => {
    proc.resourcesPath = path.join(tmpRoot, 'nope');
    const { readFeedUrl } = await loadFresh();
    expect(readFeedUrl()).toBeNull();
    proc.resourcesPath = tmpRoot;
    writeYml('https://feed.example.com');
    expect(readFeedUrl()).toBe('https://feed.example.com');
  });
});
