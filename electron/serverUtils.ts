// Shared helpers for spawning local model servers (llama.cpp / whisper.cpp) and
// downloading their weights. Mirrors the patterns originally inlined in
// analyzer.js so a second server (electron/stt.js) can reuse them verbatim.

import fs from 'fs';
import net from 'net';
import path from 'path';
import crypto from 'crypto';
import type { AddressInfo } from 'net';

// Hosts we're willing to fetch model weights from. huggingface.co serves the
// metadata/redirect; the actual blob is served from its CDN on *.hf.co /
// cdn-lfs* hosts. We allow these and reject any cross-origin redirect to an
// unexpected host (defense against a tampered/MITM'd redirect chain).
const MODEL_HOST_ALLOWLIST = ['huggingface.co', 'hf.co'];
const MODEL_HOST_SUFFIX_ALLOWLIST = ['.huggingface.co', '.hf.co'];

function isAllowedModelHost(hostname: string): boolean {
  const h = (hostname || '').toLowerCase();
  if (MODEL_HOST_ALLOWLIST.includes(h)) return true;
  return MODEL_HOST_SUFFIX_ALLOWLIST.some((s) => h.endsWith(s));
}

// First path in the list that exists on disk, or null. Skips falsy entries so
// callers can pass optional env-var overrides directly.
function firstExisting(paths: Array<string | undefined>): string | null {
  for (const p of paths) {
    if (p && fs.existsSync(p)) return p;
  }
  return null;
}

// Asks the OS for a free ephemeral port on the loopback interface.
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address() as AddressInfo;
      s.close(() => resolve(port));
    });
    s.on('error', reject);
  });
}

// Polls an HTTP endpoint until it answers 2xx or the timeout elapses. `path`
// lets callers pick a health route ('/health' for llama-server, '/' for
// whisper-server which has no dedicated health endpoint).
async function waitForHttp(port: number, healthPath: string, timeout: number): Promise<void> {
  const t0 = Date.now();
  while (Date.now() - t0 < timeout) {
    try {
      // Bound each probe so a connection that's accepted but never answered
      // (e.g. a child stuck loading the model) can't hang past the deadline:
      // the abort makes fetch reject, the loop re-checks the overall timeout.
      const r = await fetch(`http://127.0.0.1:${port}${healthPath}`, {
        signal: AbortSignal.timeout(2000),
      });
      if (r.ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`server not ready within ${timeout}ms`);
}

// Hashes a file on disk to a lowercase hex SHA256 without buffering it in RAM.
function sha256File(file: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const h = crypto.createHash('sha256');
    const s = fs.createReadStream(file);
    s.on('error', reject);
    s.on('data', (d) => h.update(d));
    s.on('end', () => resolve(h.digest('hex')));
  });
}

interface DownloadFileOptions {
  keepPartialOnAbort?: () => boolean;
  expectedSha?: string;
}

// Streams one file to disk with progress, into a .part then rename on success.
// onProgress receives a 0..1 fraction (only when content-length is known).
// Resumes from an existing .part via an HTTP Range request when possible. The
// optional `keepPartialOnAbort` predicate, when it returns true at abort/error
// time, preserves the .part so a paused download can resume; otherwise the
// orphan .part is removed.
async function downloadFile(
  url: string,
  dest: string,
  onProgress: ((fraction: number) => void) | undefined,
  signal: AbortSignal | undefined,
  { keepPartialOnAbort, expectedSha }: DownloadFileOptions = {},
): Promise<void> {
  const tmp = `${dest}.part`;

  // Resume from a prior partial download when one is present.
  let startByte = 0;
  try {
    startByte = fs.statSync(tmp).size;
  } catch {
    startByte = 0;
  }

  const headers: Record<string, string> = startByte > 0 ? { Range: `bytes=${startByte}-` } : {};
  const res = await fetch(url, { redirect: 'follow', signal, headers });
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status} for ${path.basename(dest)}`);

  // Constrain the *final* URL (after redirects). A model download must end on
  // an https host in our allowlist; a redirect that lands us on http:// or an
  // unexpected host (e.g. via a poisoned redirect) is rejected before we write
  // anything to disk.
  try {
    const finalUrl = new URL(res.url || url);
    if (finalUrl.protocol !== 'https:') {
      throw new Error(`Refusing non-https model URL: ${finalUrl.protocol}`);
    }
    if (!isAllowedModelHost(finalUrl.hostname)) {
      throw new Error(`Refusing model download from unexpected host: ${finalUrl.hostname}`);
    }
  } catch (e) {
    if (e instanceof TypeError) throw new Error(`Invalid model URL: ${url}`);
    throw e;
  }

  // We asked to resume but the server ignored the Range (200 not 206): start over.
  const resuming = startByte > 0 && res.status === 206;
  if (startByte > 0 && !resuming) {
    try {
      fs.unlinkSync(tmp);
    } catch {}
    startByte = 0;
  }

  // On a 206 the content-length is the REMAINING bytes; full size = prefix + rest.
  const remaining = Number(res.headers.get('content-length')) || 0;
  const total = resuming ? startByte + remaining : remaining;

  const out = fs.createWriteStream(tmp, { flags: resuming ? 'a' : 'w' });
  let received = startByte;
  try {
    for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array | string>) {
      // Respect backpressure so a stalled disk doesn't balloon memory.
      if (!out.write(chunk)) {
        await new Promise<void>((resolve, reject) => {
          // Pair the listeners so they clean each other up: on the normal
          // 'drain' path the dormant 'error' listener (and vice versa) is
          // removed, otherwise one stranded listener would accumulate on the
          // stream per backpressure event over a multi-GB download.
          const onDrain = () => {
            out.removeListener('error', onError);
            resolve();
          };
          const onError = (e: Error) => {
            out.removeListener('drain', onDrain);
            reject(e);
          };
          out.once('drain', onDrain);
          out.once('error', onError);
        });
      }
      received += chunk.length;
      if (total) onProgress?.(received / total);
    }
    await new Promise<void>((resolve, reject) =>
      out.end((err?: Error | null) => (err ? reject(err) : resolve())),
    );
    // Size match against content-length catches truncated/altered downloads
    // (upstream SHA256 isn't known ahead of time).
    if (total && received !== total) {
      throw new Error(`Truncated download for ${path.basename(dest)}: ${received}/${total} bytes`);
    }
    // Without a content-length we can't validate the size: at least require a
    // non-empty body and flag that the integrity check was skipped.
    if (!total) {
      if (received <= 0) {
        throw new Error(`Empty download for ${path.basename(dest)} (no content-length)`);
      }
      console.warn(
        `[serverUtils] no content-length for ${path.basename(dest)}: cannot validate size (${received} bytes received)`,
      );
    }
    // SHA256 integrity check when a known digest is supplied. We never fabricate
    // an expected hash; this only runs if the caller passes one. The .part is
    // hashed in place before the rename so a corrupted file is never promoted.
    // TODO: populate `expectedSha` from a vetted hash list in the MODELS/config
    //       registries (e.g. pinned per-file SHA256 from the HF repo) so model
    //       downloads are integrity-checked, not just size-checked.
    if (expectedSha) {
      const actual = await sha256File(tmp);
      if (actual.toLowerCase() !== String(expectedSha).toLowerCase()) {
        throw new Error(
          `SHA256 mismatch for ${path.basename(dest)}: expected ${expectedSha}, got ${actual}`,
        );
      }
    }
    fs.renameSync(tmp, dest);
  } catch (err) {
    // Tear down the open fd. Keep the .part only when the caller is pausing, so
    // the next run can resume; on cancel/error drop it.
    out.destroy();
    if (!(typeof keepPartialOnAbort === 'function' && keepPartialOnAbort())) {
      try {
        fs.unlinkSync(tmp);
      } catch {}
    }
    throw err;
  }
}

export { firstExisting, freePort, waitForHttp, downloadFile, isAllowedModelHost, sha256File };
