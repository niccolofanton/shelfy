import { describe, it, expect } from 'vitest';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import {
  CaptureRequestSchema,
  ManifestSchema,
  CaptureLineSchema,
  ASSET_FILE_RE,
  manifestJsonSchema,
} from '../src/protocol';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');
const FIXTURE = path.join(HERE, '..', 'fixtures', 'recorded', 'basic');

const VALID = {
  captureId: '01J0000000000000000000000X',
  url: 'https://example.com/',
  maxPages: 6,
  singlePage: false,
  video: true,
  workDir: '/work/01J0000000000000000000000X',
};

describe('CaptureRequestSchema', () => {
  it('accepts a well-formed request', () => {
    expect(CaptureRequestSchema.safeParse(VALID).success).toBe(true);
  });

  it('rejects a non-ULID captureId', () => {
    expect(CaptureRequestSchema.safeParse({ ...VALID, captureId: 'not-a-ulid' }).success).toBe(
      false,
    );
  });

  it('rejects a non-http(s) url and an over-long url', () => {
    expect(CaptureRequestSchema.safeParse({ ...VALID, url: 'file:///etc/passwd' }).success).toBe(
      false,
    );
    expect(
      CaptureRequestSchema.safeParse({ ...VALID, url: 'https://e.com/' + 'a'.repeat(2048) })
        .success,
    ).toBe(false);
  });

  it('rejects maxPages out of 1..8', () => {
    expect(CaptureRequestSchema.safeParse({ ...VALID, maxPages: 0 }).success).toBe(false);
    expect(CaptureRequestSchema.safeParse({ ...VALID, maxPages: 9 }).success).toBe(false);
  });

  it('requires workDir === /work/<captureId>', () => {
    expect(CaptureRequestSchema.safeParse({ ...VALID, workDir: '/tmp/evil' }).success).toBe(false);
    expect(
      CaptureRequestSchema.safeParse({ ...VALID, workDir: '/work/01J0000000000000000000000Y' })
        .success,
    ).toBe(false);
  });
});

describe('asset file names', () => {
  it('the recorded fixture asset names all match the safe pattern', () => {
    for (const f of fs.readdirSync(FIXTURE)) {
      if (f === 'lines.ndjson' || f === 'manifest.json') continue;
      expect(ASSET_FILE_RE.test(f), f).toBe(true);
    }
  });

  it('a path traversal / dotted host name is refused', () => {
    expect(ASSET_FILE_RE.test('../secret.webp')).toBe(false);
    expect(ASSET_FILE_RE.test('1696-linear.app-abcd.webp')).toBe(false);
    expect(ASSET_FILE_RE.test('p0-hero.svg')).toBe(false);
  });
});

describe('recorded fixture', () => {
  it('its manifest.json validates against ManifestSchema', () => {
    const manifest = JSON.parse(fs.readFileSync(path.join(FIXTURE, 'manifest.json'), 'utf8'));
    const res = ManifestSchema.safeParse(manifest);
    if (!res.success) console.error(res.error.issues.slice(0, 5));
    expect(res.success).toBe(true);
  });

  it('every page line asset references an existing file matching the pattern', () => {
    const lines = fs
      .readFileSync(path.join(FIXTURE, 'lines.ndjson'), 'utf8')
      .split('\n')
      .filter((l) => l.trim())
      .map((l) => CaptureLineSchema.parse(JSON.parse(l)));
    const pages = lines.filter((l) => l.type === 'page');
    expect(pages.length).toBeGreaterThan(0);
    for (const p of pages) {
      if (p.type !== 'page') continue;
      for (const a of p.assets) {
        expect(ASSET_FILE_RE.test(a.file), a.file).toBe(true);
        expect(fs.existsSync(path.join(FIXTURE, a.file)), a.file).toBe(true);
      }
    }
    expect(lines.at(-1)?.type).toBe('done');
  });
});

describe('generated contract', () => {
  it('capture/protocol.schema.json is in sync with the zod manifest schema', () => {
    // Compare parsed objects, not bytes: the committed file is prettier-formatted,
    // but it must stay semantically equal to the generated schema.
    const onDisk = JSON.parse(
      fs.readFileSync(path.join(ROOT, 'capture', 'protocol.schema.json'), 'utf8'),
    );
    expect(onDisk).toEqual(manifestJsonSchema());
  });
});
