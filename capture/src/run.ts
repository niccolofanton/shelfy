// Run ONE site capture for the service: drive capture v2 (electron/webcap) with
// the server budgets, assemble the design metadata (shared assemble.ts), relocate
// the artifacts to short role-based names, write manifest.json, and stream the
// §2.18 NDJSON (event / page / done | failed).

import fs from 'fs';
import path from 'path';
import { captureSite, BlockedError, PageError } from '../../electron/webcap/capture';
import type { SiteCapture, CapturedPage, CaptureEvent } from '../../electron/webcap/capture';
import { assembleSite, pickIcon } from '../../electron/webcap/assemble';
import type { SiteAssembly } from '../../electron/webcap/assemble';
import { fetchImage, blockedFallback } from './fetched';
import { cancelBrowserIdle, armBrowserIdleClose } from '../../electron/webcap/browser';
import { ENV, type CaptureEnv } from './env';
import {
  ASSET_FILE_RE,
  type Asset,
  type CaptureLine,
  type FailureCode,
  type Manifest,
  type Params,
} from './protocol';
import type { CaptureRequest } from './protocol';

export type EmitLine = (line: CaptureLine) => void;

export interface RunContext {
  emit: EmitLine;
  signal: AbortSignal;
  env?: CaptureEnv;
}

interface ProbeHead {
  ogImage?: string;
  icons?: { href: string; rel: string; sizes: string; type: string }[];
}

function ext(file: string): string {
  return path.extname(file).replace(/^\./, '').toLowerCase() || 'webp';
}

// Move a capture-v2 asset (named <stamp>-<host>-<hash>.webp under assets/web) to a
// short, safe, role-based name at the work-dir root, the name P4-14 opens with
// O_NOFOLLOW and matches against ASSET_FILE_RE.
function relocate(
  workDir: string,
  srcPath: string | null | undefined,
  safeBase: string,
): Asset | null {
  if (!srcPath) return null;
  const e = ext(srcPath);
  const file = `${safeBase}.${e === 'jpeg' ? 'jpg' : e}`;
  if (!ASSET_FILE_RE.test(file)) return null;
  const dest = path.join(workDir, file);
  try {
    if (!fs.existsSync(srcPath)) return null;
    if (path.resolve(srcPath) !== path.resolve(dest)) fs.renameSync(srcPath, dest);
  } catch {
    try {
      fs.copyFileSync(srcPath, dest);
    } catch {
      return null;
    }
  }
  return { role: safeBase, file, w: 0, h: 0 };
}

// Build a page's relocated asset list and return both the assets and the cover
// role names for page 0.
function relocatePage(workDir: string, p: CapturedPage, index: number): Asset[] {
  const out: Asset[] = [];
  const push = (a: Asset | null, w: number, h: number, extra?: Partial<Asset>): void => {
    if (a) out.push({ ...a, w, h, ...extra });
  };
  if (p.hero) push(relocate(workDir, p.hero.path, `p${index}-hero`), p.hero.width, p.hero.height);
  p.bands.forEach((b, i) =>
    push(relocate(workDir, b.path, `p${index}-band${i}`), b.width, b.height, {
      seq: i,
      top: b.top,
      cssHeight: b.cssHeight,
    }),
  );
  if (p.footer)
    push(relocate(workDir, p.footer.path, `p${index}-footer`), p.footer.width, p.footer.height);
  p.sections.forEach((s, i) =>
    push(relocate(workDir, s.path, `p${index}-sec${i}`), s.width, s.height, {
      seq: i,
      top: s.top,
      cssHeight: s.cssHeight,
    }),
  );
  if (p.video) {
    push(relocate(workDir, p.video.path, `p${index}-video`), p.video.width, p.video.height);
    if (p.video.preview) push(relocate(workDir, p.video.preview, `p${index}-preview`), 0, 0);
  }
  return out;
}

function failureFor(err: unknown, aborted: boolean): FailureCode {
  if (err instanceof BlockedError) return 'capture_blocked';
  if (err instanceof PageError) {
    if (err.code === 'timeout') return 'timeout';
    if (err.code === 'empty') return 'empty';
    return 'navigation'; // http / not-html / navigation
  }
  if (aborted) return 'timeout';
  return 'internal';
}

function sumBytes(workDir: string, files: string[]): number {
  let total = 0;
  for (const f of files) {
    try {
      total += fs.statSync(path.join(workDir, f)).size;
    } catch {}
  }
  return total;
}

export async function runCapture(req: CaptureRequest, ctx: RunContext): Promise<void> {
  const env = ctx.env ?? ENV;
  // The request carries the logical /work/<captureId>; the physical dir is under
  // env.workBase (== /work in production, a temp dir in tests).
  const workDir = path.join(env.workBase, req.captureId);
  const T0 = Date.now();
  process.env.CAPTURE_WORK_DIR = workDir;
  fs.mkdirSync(workDir, { recursive: true });
  cancelBrowserIdle();

  // Peak RSS over the run (reported in `done`; P4-14 clamps it).
  let peakRss = process.memoryUsage().rss;
  const rssTimer = setInterval(() => {
    peakRss = Math.max(peakRss, process.memoryUsage().rss);
  }, 250);
  rssTimer.unref();

  // Capped emit: at most env.maxEvents event lines, each at most env.maxLineBytes;
  // page / done / failed always pass. Collect the timeline (≤ maxEvents codes).
  let eventCount = 0;
  const timeline: { kind: string; code: string; params?: Params }[] = [];
  const emit = (line: CaptureLine): void => {
    if (line.type === 'event') {
      if (eventCount >= env.maxEvents) return;
      let out = line;
      if (Buffer.byteLength(JSON.stringify(out)) > env.maxLineBytes) {
        out = { ...line, params: undefined };
      }
      eventCount++;
      if (timeline.length < env.maxEvents)
        timeline.push({ kind: out.kind, code: out.code, params: out.params });
      ctx.emit(out);
      return;
    }
    ctx.emit(line);
  };

  const hooks = {
    onEvent: (e: CaptureEvent): void => {
      emit({ type: 'event', kind: e.kind, code: e.code, params: e.params });
    },
    // Stage codes go to the user's stream as info events; P4-14 routes them to
    // job.updated.
    onStage: (stage: string, frac: number): void => {
      emit({ type: 'event', kind: 'info', code: 'stage', params: { stage, frac } });
    },
  };

  let site: SiteCapture;
  try {
    site = await captureSite(req.url, {
      maxPages: req.maxPages,
      singlePage: req.singlePage,
      stamp: Math.floor(Date.now() / 1000),
      // O6: the scroll video stays behind the service setting (off by default).
      video: req.video && env.video,
      signal: ctx.signal,
      pagesParallel: env.pagesParallel,
      deviceScale: env.deviceScale,
      encodePoolSize: env.encodePoolSize,
      primaryDeadlineMs: env.primaryDeadlineMs,
      innerDeadlineMs: env.innerDeadlineMs,
      siteBudgetMs: env.siteBudgetMs,
      maxArtifactBytes: env.maxArtifactBytes,
      sessionLocale: process.env.CAPTURE_LOCALE,
      // No fallback: a Playwright launch failure fails the capture on the server.
      hooks,
    });
  } catch (err) {
    clearInterval(rssTimer);
    armBrowserIdleClose();
    if (ctx.signal.aborted) return; // client closed the stream → stay silent
    if ((err as Error)?.name === 'AbortError') return;
    if (err instanceof BlockedError) await blockedFallback(req, workDir, ctx.signal, T0, peakRss);
    if (ctx.signal.aborted) return;
    emit({ type: 'failed', code: failureFor(err, ctx.signal.aborted) });
    return;
  }

  try {
    const primary = site.primary;
    const finalUrl = primary.url;
    const head = (primary.probe.head || {}) as ProbeHead;

    // og:image + favicon through the proxy; path-free in the manifest (role file).
    const ogPath = head.ogImage
      ? await fetchImage(head.ogImage, finalUrl, workDir, 'og', ctx.signal)
      : null;
    const iconUrl = pickIcon(head.icons || [], finalUrl);
    const favPath = iconUrl
      ? await fetchImage(iconUrl, finalUrl, workDir, 'favicon', ctx.signal)
      : null;

    const assembly: SiteAssembly = await assembleSite(site, {
      ogImageUrl: head.ogImage,
      ogFetched: !!ogPath,
      singlePage: req.singlePage,
      signal: ctx.signal,
    });

    // Relocate artifacts to safe role-based names, build the page lines.
    const pageAssets: Asset[][] = site.pages.map((p, i) => relocatePage(workDir, p, i));
    // og/favicon dimensions stay 0×0 placeholders: P4-14 sniffs the real type and
    // size on ingest.
    const og = relocate(workDir, ogPath, 'og');
    const favicon = relocate(workDir, favPath, 'favicon');
    let domain = '';
    try {
      domain = new URL(finalUrl).hostname.replace(/^www\./, '');
    } catch {}

    const coverRole =
      assembly.cover === 'og' ? 'og' : assembly.cover === 'band0' ? 'p0-band0' : 'p0-hero';

    const manifestPages: Manifest['pages'] = site.pages.map((p, i) => {
      const ap = assembly.pages[i];
      return {
        index: i,
        url: ap.url,
        requestedUrl: ap.requestedUrl,
        pageType: ap.pageType,
        title: ap.title,
        status: ap.status,
        heightCss: ap.heightCss,
        capped: ap.capped,
        jacked: ap.jacked,
        qc: ap.qc,
        contentText: ap.contentText,
        digest: ap.digest,
        sections: ap.sections,
        assets: pageAssets[i],
      };
    });

    const partial = site.skipped.some((s) => s.reason === 'budget_exceeded');
    const manifest: Manifest = {
      schema: 2,
      version: 1,
      url: req.url,
      finalUrl,
      domain,
      title: assembly.title,
      siteName: assembly.siteName,
      description: assembly.description,
      lang: assembly.lang,
      languages: assembly.languages,
      engine: site.engine,
      userAgent: site.userAgent,
      viewport: assembly.meta.capture.viewport,
      palette: assembly.palette as unknown as Record<string, unknown>[],
      scheme: assembly.scheme,
      contrast: assembly.contrast as unknown as Record<string, unknown> | null,
      typography: {
        fonts: assembly.fonts as unknown as Record<string, unknown>[],
        scale: assembly.meta.typeScale as unknown as Record<string, unknown>[],
        baseSize: assembly.meta.baseSize,
        ratio: assembly.meta.scaleRatio,
      },
      tech: assembly.meta.tech as unknown as Record<string, unknown>[],
      traits: assembly.meta.traits as unknown as Record<string, unknown>,
      awards: assembly.awards as unknown as Record<string, unknown>[],
      awardTags: assembly.meta.awardTags,
      awardEntities: assembly.meta.awardEntities,
      jsonldTypes: assembly.meta.jsonldTypes,
      organization: assembly.meta.organization,
      social: assembly.meta.social as unknown as Record<string, unknown>[],
      credits: assembly.meta.credits as unknown as Record<string, unknown>[],
      webMeta: assembly.meta as unknown as Record<string, unknown>,
      cover: { role: coverRole },
      og: og ? { file: og.file, w: og.w, h: og.h } : null,
      favicon: favicon ? { file: favicon.file, w: favicon.w, h: favicon.h } : null,
      pages: manifestPages,
      skipped: site.skipped,
      qc: site.pages.map((p, i) => ({ index: i, status: p.qc.status, reason: p.qc.reason })),
      timeline: timeline.slice(0, env.maxEvents),
      durationMs: 0,
      peakRssBytes: 0,
      bytes: 0,
      partial,
    };

    // Collect every relocated file for the byte total (artifacts kept on disk).
    const files: string[] = [];
    for (const list of pageAssets) for (const a of list) files.push(a.file);
    if (og) files.push(og.file);
    if (favicon) files.push(favicon.file);
    const bytes = sumBytes(workDir, files);
    const durationMs = Date.now() - T0;

    manifest.durationMs = durationMs;
    manifest.peakRssBytes = peakRss;
    manifest.bytes = bytes;

    // manifest.json is written BEFORE `done`, so the API can finish the ingest if
    // the stream breaks after completion.
    fs.writeFileSync(path.join(workDir, 'manifest.json'), JSON.stringify(manifest));

    // The leftover capture-v2 asset dir (files were moved out) — remove it.
    fs.rmSync(path.join(workDir, 'assets'), { recursive: true, force: true });

    // Stream the page lines, then done.
    for (const p of manifestPages) {
      emit({ type: 'page', index: p.index, url: p.url, pageType: p.pageType, assets: p.assets });
    }
    emit({
      type: 'done',
      manifest: 'manifest.json',
      durationMs,
      peakRssBytes: peakRss,
      bytes,
      partial,
    });
  } catch (err) {
    if (ctx.signal.aborted || (err as Error)?.name === 'AbortError') return;
    emit({ type: 'failed', code: 'internal' });
  } finally {
    clearInterval(rssTimer);
    armBrowserIdleClose();
  }
}
