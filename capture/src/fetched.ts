// Service-only strict fetches: a partial/oversized OG response is never encoded.
// Each redirect goes through the SSRF guard; the production proxy guards DNS.
import fs from 'fs';
import path from 'path';
import { assertSafeUrl } from '../../electron/net-safety';
import { encodeImage } from '../../electron/webcap/sitefetch';
import type { Manifest, CaptureRequest } from './protocol';

export const FETCH_CAP = 2 * 1024 * 1024;
export async function fetchComplete(
  url: string,
  signal: AbortSignal,
  accept: string,
): Promise<Buffer> {
  const deadline = AbortSignal.any([signal, AbortSignal.timeout(10_000)]);
  for (let hop = 0; hop <= 5; hop++) {
    const safe = assertSafeUrl(url);
    const response = await fetch(safe.toString(), {
      signal: deadline,
      redirect: 'manual',
      headers: { Accept: accept },
    });
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      await response.body?.cancel();
      const next = response.headers.get('location');
      if (!next) throw new Error('capture_fetch_redirect');
      url = new URL(next, safe).toString();
      continue;
    }
    if (!response.ok || !response.body) {
      await response.body?.cancel();
      throw new Error('capture_fetch_failed');
    }
    const declaredLength = Number(response.headers.get('content-length'));
    const length = response.headers.has('content-encoding') ? 0 : declaredLength;
    if (declaredLength > FETCH_CAP) {
      await response.body.cancel();
      throw new Error('capture_fetch_oversize');
    }
    const reader = response.body.getReader();
    const parts: Buffer[] = [];
    let size = 0;
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > FETCH_CAP) throw new Error('capture_fetch_oversize');
        parts.push(Buffer.from(value));
      }
      if (!size || (length > 0 && size !== length)) throw new Error('capture_fetch_truncated');
      return Buffer.concat(parts, size);
    } finally {
      await reader.cancel().catch(() => {});
    }
  }
  throw new Error('capture_fetch_redirect');
}

export async function fetchImage(
  imageUrl: string,
  pageUrl: string,
  workDir: string,
  role: 'og' | 'favicon',
  signal: AbortSignal,
): Promise<string | null> {
  const source = path.join(workDir, `${role}-source`);
  const output = path.join(workDir, `${role}.webp`);
  try {
    const bytes = await fetchComplete(new URL(imageUrl, pageUrl).toString(), signal, 'image/*');
    fs.writeFileSync(source, bytes, { flag: 'wx' });
    await encodeImage(source, output, 'webp', role === 'favicon' ? 92 : 82, signal);
    return output;
  } catch {
    fs.rmSync(output, { force: true });
    return null;
  } finally {
    fs.rmSync(source, { force: true });
  }
}

function unescape(value: string): string {
  return value
    .replace(/&amp;/gi, '&')
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/g, "'")
    .replace(/&lt;/gi, '<')
    .replace(/&gt;/gi, '>');
}
export function ogUrl(html: string, base: string): string | null {
  for (const tag of html.match(/<meta\b[^>]*>/gi) || []) {
    const attributes = new Map<string, string>();
    for (const attribute of tag.matchAll(/([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/g))
      attributes.set(
        attribute[1].toLowerCase(),
        unescape(attribute[2] ?? attribute[3] ?? attribute[4]),
      );
    if ((attributes.get('property') || attributes.get('name'))?.toLowerCase() === 'og:image') {
      try {
        return assertSafeUrl(new URL(attributes.get('content') || '', base).toString()).toString();
      } catch {
        return null;
      }
    }
  }
  return null;
}

export async function blockedFallback(
  req: CaptureRequest,
  workDir: string,
  signal: AbortSignal,
  started: number,
  peakRssBytes: number,
): Promise<boolean> {
  try {
    const html = (await fetchComplete(req.url, signal, 'text/html')).toString('utf8');
    const url = ogUrl(html, req.url);
    if (!url || !(await fetchImage(url, req.url, workDir, 'og', signal))) return false;
    const bytes = fs.statSync(path.join(workDir, 'og.webp')).size;
    const manifest: Manifest = {
      schema: 2,
      version: 1,
      url: req.url,
      finalUrl: req.url,
      domain: new URL(req.url).hostname,
      title: '',
      siteName: '',
      description: '',
      lang: null,
      languages: [],
      engine: 'playwright',
      userAgent: '',
      viewport: { width: 1440, height: 900, scale: 2 },
      palette: [],
      scheme: null,
      contrast: null,
      typography: { fonts: [], scale: [], baseSize: null, ratio: null },
      tech: [],
      traits: {},
      awards: [],
      awardTags: [],
      awardEntities: [],
      jsonldTypes: [],
      organization: null,
      social: [],
      credits: [],
      webMeta: {},
      cover: { role: 'og' },
      og: { file: 'og.webp', w: 0, h: 0 },
      favicon: null,
      pages: [],
      skipped: [],
      qc: [],
      timeline: [],
      durationMs: Date.now() - started,
      peakRssBytes,
      bytes,
      partial: true,
    };
    fs.writeFileSync(path.join(workDir, 'manifest.json'), JSON.stringify(manifest));
    return true;
  } catch {
    return false;
  }
}
