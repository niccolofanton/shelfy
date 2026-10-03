import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { runFake } from '../src/fake';
import { ENV } from '../src/env';
import { ManifestSchema, type CaptureLine } from '../src/protocol';

const CAPTURE_ID = '01J0000000000000000000000X';
let base: string;

beforeEach(() => {
  base = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-fake-'));
});
afterEach(() => {
  fs.rmSync(base, { recursive: true, force: true });
});

function req() {
  return {
    captureId: CAPTURE_ID,
    url: 'https://example.com/',
    maxPages: 6,
    singlePage: false,
    video: true,
    workDir: `/work/${CAPTURE_ID}`,
  };
}

describe('runFake', () => {
  it('replays the recorded stream and lands the artifacts in the work dir', async () => {
    const lines: CaptureLine[] = [];
    await runFake(
      req(),
      {
        emit: (l) => lines.push(l),
        signal: new AbortController().signal,
        env: { ...ENV, workBase: base },
      },
      { fixture: 'basic' },
    );

    // The stream: events, at least one page, ending in done.
    expect(lines[0].type).toBe('event');
    expect(lines.some((l) => l.type === 'page')).toBe(true);
    const done = lines.at(-1);
    expect(done?.type).toBe('done');

    // manifest + every page asset copied into the physical work dir.
    const workDir = path.join(base, CAPTURE_ID);
    expect(fs.existsSync(path.join(workDir, 'manifest.json'))).toBe(true);
    const manifest = JSON.parse(fs.readFileSync(path.join(workDir, 'manifest.json'), 'utf8'));
    expect(ManifestSchema.safeParse(manifest).success).toBe(true);
    for (const l of lines) {
      if (l.type !== 'page') continue;
      for (const a of l.assets)
        expect(fs.existsSync(path.join(workDir, a.file)), a.file).toBe(true);
    }
  });

  it('stops early when the signal is aborted', async () => {
    const ac = new AbortController();
    ac.abort();
    const lines: CaptureLine[] = [];
    await runFake(
      req(),
      { emit: (l) => lines.push(l), signal: ac.signal, env: { ...ENV, workBase: base } },
      { fixture: 'basic' },
    );
    expect(lines.length).toBe(0);
  });
});
