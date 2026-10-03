// Replay a recorded capture: copy the recorded artifacts into the request's work
// dir and stream its recorded NDJSON, so the API and web e2e tests (and P4-14's
// in-process fake) exercise the whole ingest path with no browser, ffmpeg or
// network. A recorded fixture lives in capture/fixtures/recorded/<name>/ as
// lines.ndjson + manifest.json + the role-named asset files.

import fs from 'fs';
import path from 'path';
import type { EmitLine, RunContext } from './run';
import { ENV } from './env';
import { CaptureLineSchema, type CaptureLine } from './protocol';
import type { CaptureRequest } from './protocol';

// Recorded fixtures live at capture/fixtures/recorded (repo) or dist/fixtures/
// recorded (bundle). The bundled image sets CAPTURE_RECORDED_DIR; locally the
// default resolves from the repo root (cwd). Kept free of import.meta.url so the
// CJS bundle has no "import.meta" warning.
export function recordedDir(): string {
  return (
    process.env.CAPTURE_RECORDED_DIR ||
    path.resolve(process.cwd(), 'capture', 'fixtures', 'recorded')
  );
}

export interface FakeOptions {
  dir?: string; // recorded fixtures root
  fixture?: string; // which recording (default from env, else "basic")
}

// Read and validate the recorded NDJSON lines of a fixture.
export function readRecordedLines(dir: string): CaptureLine[] {
  const raw = fs.readFileSync(path.join(dir, 'lines.ndjson'), 'utf8');
  return raw
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => CaptureLineSchema.parse(JSON.parse(l)));
}

export async function runFake(
  req: CaptureRequest,
  ctx: RunContext,
  opts: FakeOptions = {},
): Promise<void> {
  const base = opts.dir || recordedDir();
  const fixture = opts.fixture || process.env.SHELFY_CAPTURE_FAKE_FIXTURE || 'basic';
  const dir = path.join(base, fixture);
  const lines = readRecordedLines(dir);

  // Copy every recorded artifact into the physical work dir under its recorded
  // name (env.workBase/<captureId>; == req.workDir in production).
  const workDir = path.join((ctx.env ?? ENV).workBase, req.captureId);
  fs.mkdirSync(workDir, { recursive: true });
  for (const name of fs.readdirSync(dir)) {
    if (name === 'lines.ndjson') continue;
    fs.copyFileSync(path.join(dir, name), path.join(workDir, name));
  }

  const emit: EmitLine = ctx.emit;
  for (const line of lines) {
    if (ctx.signal.aborted) return;
    emit(line);
  }
}
