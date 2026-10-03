// SPIKE-11 harness: runs capture v2 (electron/webcap/*) for one site in plain
// Node, the way the P4 capture service will (plan §2.18), and records what it
// cost. Built by build.mjs into .scratch/out/run-site.cjs, with `electron` replaced by
// src/electron-shim.ts.
//
// Phases, as in electron/weborchestrator.ts captureWebReference() minus the DB
// and AI steps: captureSite() (primary page with scroll video, page selection,
// inner pages), then the design metadata (palette from pixels, typography,
// tech, awards, traits, page digests) and the og:image and favicon downloads.
//
// Output in --out: assets/web/* (the artifacts), events.ndjson (the §2.18
// streaming protocol: event, page, done | failed lines), manifest.json (pages
// and assets), metrics.json (wall time, CPU, memory, network, output size).
//
// Usage:
//   node .scratch/out/run-site.cjs --url <url> --out <dir> [--label <name>]
//     [--max-pages 6] [--single-page] [--no-video] [--site-budget-s 600]
//     [--fixtures <dir>]            serve https://fixtures.shelfy.test/<file>.html from <dir>
//     [--page-concurrency N]        inner pages captured at once (default: capture v2's
//                                   own choice, 3, or 2 for WebGL / scroll-jacked sites)
//     [--dpr N]                     device scale (default 2, as capture v2; cost comparisons)
//
// Environment:
//   CAPTURE_PROXY      egress proxy for Chromium, e.g. http://shelfy-egress:4750
//   NODE_USE_ENV_PROXY=1 with HTTP_PROXY and HTTPS_PROXY: the same proxy for
//                      Node's fetch (discovery, og:image); required with CAPTURE_PROXY
//   FFMPEG_BIN         ffmpeg with libwebp and libx264
//   PLAYWRIGHT_BROWSERS_PATH  where chromium-headless-shell is installed
//   CAPTURE_LOCALE     browser locale (default en-US)
//   SHELFY_DISABLE_SANDBOX=1  turns Chromium's sandbox off (diagnostics only)

import fs from 'fs';
import path from 'path';
import { captureSite, BlockedError, PageError } from '../../../../electron/webcap/capture';
import type { CapturedPage, SiteCapture, CaptureEvent } from '../../../../electron/webcap/capture';
import * as meta from '../../../../electron/webcap/metadata';
import * as webcapture from '../../../../electron/webcapture';
import { closeBrowser } from '../../../../electron/webcapture-playwright';
import { patchChromium, effectiveLaunch, sandboxReport } from './chromium';
import { createSession } from './session';
import type { SandboxProcess } from './chromium';
import {
  Sampler,
  cpuStat,
  memoryEvents,
  memoryPeak,
  netBytes,
  dirBytes,
  cgroupLimits,
} from './metrics';

interface Args {
  url: string;
  out: string;
  label: string;
  maxPages: number;
  singlePage: boolean;
  video: boolean;
  siteBudgetS: number;
  fixtures: string | null;
  pageConcurrency: number | null;
  dpr: number | null;
}

function parseArgs(argv: string[]): Args {
  const a: Args = {
    url: '',
    out: '',
    label: '',
    maxPages: 6,
    singlePage: false,
    video: true,
    siteBudgetS: 600,
    fixtures: null,
    pageConcurrency: null,
    dpr: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const v = (): string => {
      const x = argv[++i];
      if (x === undefined) throw new Error(`${k} needs a value`);
      return x;
    };
    if (k === '--url') a.url = v();
    else if (k === '--out') a.out = path.resolve(v());
    else if (k === '--label') a.label = v();
    else if (k === '--max-pages') a.maxPages = Number(v());
    else if (k === '--single-page') a.singlePage = true;
    else if (k === '--no-video') a.video = false;
    else if (k === '--site-budget-s') a.siteBudgetS = Number(v());
    else if (k === '--fixtures') a.fixtures = path.resolve(v());
    else if (k === '--page-concurrency') a.pageConcurrency = Number(v());
    else if (k === '--dpr') a.dpr = Number(v());
    else throw new Error(`unknown argument ${k}`);
  }
  if (!a.url || !a.out) throw new Error('usage: run-site.cjs --url <url> --out <dir> [options]');
  if (!a.label) a.label = new URL(a.url).hostname;
  return a;
}

function pickIcon(
  icons: { href: string; rel: string; sizes: string; type: string }[],
  pageUrl: string,
): string | null {
  // Same choice as electron/weborchestrator.ts pickIcon (not exported there).
  const scored = icons
    .filter((i) => i.href && !/^data:/.test(i.href))
    .map((i) => {
      const size = Number((/(\d+)x\d+/.exec(i.sizes || '') || [])[1]) || 0;
      const svg = /svg/.test(i.type) || /\.svg(\?|$)/i.test(i.href);
      const apple = /apple-touch-icon/i.test(i.rel);
      return { href: i.href, score: (apple ? 300 : 0) + Math.min(size, 512) + (svg ? -400 : 0) };
    })
    .sort((x, y) => y.score - x.score);
  if (scored.length) return scored[0].href;
  try {
    return new URL('/favicon.ico', pageUrl).toString();
  } catch {
    return null;
  }
}

function rel(out: string, p: string | null | undefined): string | null {
  return p ? path.relative(out, p) : null;
}

function pageAssets(
  out: string,
  p: CapturedPage,
): { role: string; file: string | null; w: number; h: number }[] {
  const list: { role: string; file: string | null; w: number; h: number }[] = [];
  if (p.hero)
    list.push({ role: 'hero', file: rel(out, p.hero.path), w: p.hero.width, h: p.hero.height });
  p.bands.forEach((b, i) =>
    list.push({ role: `band${i}`, file: rel(out, b.path), w: b.width, h: b.height }),
  );
  if (p.footer)
    list.push({
      role: 'footer',
      file: rel(out, p.footer.path),
      w: p.footer.width,
      h: p.footer.height,
    });
  p.sections.forEach((s, i) =>
    list.push({ role: `section${i}`, file: rel(out, s.path), w: s.width, h: s.height }),
  );
  if (p.video) {
    list.push({ role: 'video', file: rel(out, p.video.path), w: p.video.width, h: p.video.height });
    if (p.video.preview)
      list.push({ role: 'video-preview', file: rel(out, p.video.preview), w: 0, h: 0 });
  }
  return list;
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const T0 = Date.now();
  fs.mkdirSync(args.out, { recursive: true });
  process.env.CAPTURE_WORK_DIR = args.out;
  // The shim reports a packaged app: the adblock engine comes from here.
  Object.defineProperty(process, 'resourcesPath', {
    value: path.join(__dirname, 'resources'),
    configurable: true,
  });
  const proxy = process.env.CAPTURE_PROXY || null;
  if (proxy && process.env.NODE_USE_ENV_PROXY !== '1')
    throw new Error('CAPTURE_PROXY needs NODE_USE_ENV_PROXY=1, HTTP_PROXY and HTTPS_PROXY');
  patchChromium({ proxy, fixtureDir: args.fixtures });

  const events = fs.createWriteStream(path.join(args.out, 'events.ndjson'));
  const line = (o: unknown): void => {
    events.write(`${JSON.stringify(o)}\n`);
  };
  const sampler = new Sampler();
  sampler.start(250);
  const cpu0 = cpuStat();
  const net0 = netBytes();
  const marks: Record<string, number> = {};
  const cpuAt: Record<string, number> = {};
  const mark = (name: string): void => {
    marks[name] = Date.now() - T0;
    cpuAt[name] = ((cpuStat().usage_usec || 0) - (cpu0.usage_usec || 0)) / 1e6;
  };

  const ac = new AbortController();
  const budget = setTimeout(() => ac.abort(), args.siteBudgetS * 1000);
  let site: SiteCapture | null = null;
  let outcome: { status: 'done' | 'failed'; code?: string; message?: string } = { status: 'done' };
  // Snapshot the sandbox layers of Chromium's processes once a renderer exists.
  let sandbox: SandboxProcess[] = [];
  const sandboxTimer = setInterval(() => {
    const snap = sandboxReport();
    if (snap.some((r) => r.type === 'renderer')) {
      sandbox = snap;
      clearInterval(sandboxTimer);
    }
  }, 1000);
  sandboxTimer.unref();
  const stamp = Math.floor(Date.now() / 1000);
  let pageIndex = 0;
  let session: Awaited<ReturnType<typeof createSession>> | undefined;

  try {
    mark('start');
    if (args.pageConcurrency || args.dpr)
      session = await createSession(args.pageConcurrency ?? undefined, args.dpr ?? undefined);
    site = await captureSite(args.url, {
      maxPages: args.singlePage ? 1 : args.maxPages,
      singlePage: args.singlePage,
      stamp,
      video: args.video,
      signal: ac.signal,
      session,
      hooks: {
        onEvent: (e: CaptureEvent) => line({ type: 'event', kind: e.kind, text: e.text }),
        // First time each stage starts ('pages' fires once per inner page).
        onStage: (stage) => {
          if (!(`stage:${stage}` in marks)) mark(`stage:${stage}`);
        },
        onPage: (p) => {
          line({
            type: 'page',
            index: pageIndex++,
            url: p.url,
            pageType: p.pageType,
            assets: pageAssets(args.out, p),
          });
        },
      },
    });
    mark('captured');
  } catch (err) {
    const e = err as Error;
    if (err instanceof BlockedError)
      outcome = { status: 'failed', code: 'capture_blocked', message: e.message };
    else if (ac.signal.aborted)
      outcome = { status: 'failed', code: 'timeout', message: 'site budget' };
    else if (err instanceof PageError)
      outcome = { status: 'failed', code: 'capture_failed', message: e.message };
    else outcome = { status: 'failed', code: 'capture_failed', message: e?.message || String(err) };
    mark('captured');
  }

  // Design metadata, as the orchestrator computes it after the capture.
  let metaSummary: Record<string, unknown> | null = null;
  if (site) {
    const pages = site.pages;
    const primary = site.primary;
    const head = (primary.probe.head || {}) as {
      ogImage?: string;
      icons?: { href: string; rel: string; sizes: string; type: string }[];
      jsonld?: string[];
    };
    const domain = new URL(primary.url).hostname;
    const jsonld = meta.parseJsonLd(Array.isArray(head.jsonld) ? head.jsonld : []);
    const palette = await meta.computePalette(pages, ac.signal).catch(() => null);
    const typo = meta.computeTypography(pages, domain);
    const tech = meta.computeTech(pages);
    const awards = meta.computeAwards(pages, domain, domain);
    meta.computeTraits(pages, tech);
    meta.awardTags(awards);
    pages.forEach((p, i) => meta.pageDigest(p, i === 0 ? 1600 : 700));
    const og = head.ogImage
      ? await webcapture
          .fetchImageToWebp(head.ogImage, { pageUrl: primary.url, stamp, signal: ac.signal })
          .catch(() => null)
      : null;
    const iconUrl = pickIcon(head.icons || [], primary.url);
    const favicon = iconUrl
      ? await webcapture
          .fetchImageToWebp(iconUrl, {
            pageUrl: primary.url,
            stamp,
            quality: 92,
            signal: ac.signal,
          })
          .catch(() => null)
      : null;
    metaSummary = {
      swatches: palette?.swatches.length ?? 0,
      scheme: palette?.scheme ?? null,
      fonts: typo.fonts.length,
      tech: tech.length,
      awards: awards.length,
      jsonldTypes: jsonld.types.length,
      ogImage: rel(args.out, og),
      favicon: rel(args.out, favicon),
    };
    mark('metadata');
  }
  clearTimeout(budget);
  clearInterval(sandboxTimer);
  await session?.close();
  await closeBrowser();
  mark('end');
  sampler.stop();

  const cpu1 = cpuStat();
  const net1 = netBytes();
  const assets = dirBytes(path.join(args.out, 'assets'));
  const manifest = site
    ? {
        url: args.url,
        engine: site.engine,
        userAgent: site.userAgent,
        discovery: site.discoverySource,
        skipped: site.skipped,
        pages: site.pages.map((p) => ({
          requestedUrl: p.requestedUrl,
          url: p.url,
          pageType: p.pageType,
          status: p.status,
          title: p.title,
          heightCss: p.heightCss,
          capped: p.capped,
          jacked: p.jacked,
          qc: p.qc,
          consent: p.consent,
          overlaysRemoved: p.overlaysRemoved,
          timings: p.timings,
          requests: p.network.length,
          assets: pageAssets(args.out, p),
          video: p.video
            ? { duration: p.video.duration, w: p.video.width, h: p.video.height }
            : null,
        })),
        metadata: metaSummary,
      }
    : null;
  if (manifest)
    fs.writeFileSync(path.join(args.out, 'manifest.json'), JSON.stringify(manifest, null, 2));
  line(site ? { type: 'done', manifest: 'manifest.json' } : { type: 'failed', code: outcome.code });
  await new Promise<void>((r) => events.end(r));

  const mib = (b: number | null | undefined): number | null =>
    b === null || b === undefined ? null : Math.round((b / 1048576) * 10) / 10;
  const metrics = {
    label: args.label,
    url: args.url,
    outcome,
    options: {
      pageConcurrency: args.pageConcurrency,
      dpr: args.dpr,
      maxPages: args.maxPages,
      singlePage: args.singlePage,
      video: args.video,
      siteBudgetS: args.siteBudgetS,
    },
    pages: site?.pages.length ?? 0,
    skipped: site?.skipped.length ?? 0,
    jacked: site?.primary.jacked ?? null,
    webgl: site
      ? Boolean(
          (site.primary.probe.canvasContexts as Record<string, number> | undefined)?.webgl ||
          (site.primary.probe.canvasContexts as Record<string, number> | undefined)?.webgl2,
        )
      : null,
    videoS: site?.primary.video?.duration ?? null,
    pageTimingsS: site?.pages.map((p) => Math.round(p.timings.total / 100) / 10) ?? [],
    wallS: Math.round((Date.now() - T0) / 100) / 10,
    marksS: Object.fromEntries(
      Object.entries(marks).map(([k, v]) => [k, Math.round(v / 100) / 10]),
    ),
    cpuS: {
      total:
        cpu1.usage_usec !== undefined
          ? Math.round(((cpu1.usage_usec - (cpu0.usage_usec || 0)) / 1e6) * 10) / 10
          : null,
      user:
        cpu1.user_usec !== undefined
          ? Math.round(((cpu1.user_usec - (cpu0.user_usec || 0)) / 1e6) * 10) / 10
          : null,
      system:
        cpu1.system_usec !== undefined
          ? Math.round(((cpu1.system_usec - (cpu0.system_usec || 0)) / 1e6) * 10) / 10
          : null,
      atMarks: Object.fromEntries(
        Object.entries(cpuAt).map(([k, v]) => [k, Math.round(v * 10) / 10]),
      ),
      throttledS:
        cpu1.throttled_usec !== undefined
          ? Math.round(((cpu1.throttled_usec - (cpu0.throttled_usec || 0)) / 1e6) * 10) / 10
          : null,
      nrThrottled:
        cpu1.nr_throttled !== undefined ? cpu1.nr_throttled - (cpu0.nr_throttled || 0) : null,
    },
    memMiB: {
      peakHeld: mib(sampler.peak.held), // anon + shmem + kernel
      peakAnon: mib(sampler.peak.anon),
      peakShmem: mib(sampler.peak.shmem),
      peakCurrent: mib(sampler.peak.current),
      cgroupPeak: mib(memoryPeak()), // memory.peak since the container started
      heldAtPeak: sampler.heldAtPeak
        ? Object.fromEntries(Object.entries(sampler.heldAtPeak).map(([k, v]) => [k, mib(v)]))
        : null,
    },
    memoryEvents: memoryEvents(),
    limits: cgroupLimits(),
    netBytes: net0 && net1 ? { in: net1.rx - net0.rx, out: net1.tx - net0.tx } : null,
    output: { assetBytes: assets.bytes, assetFiles: assets.files },
    metadata: metaSummary,
    launch: effectiveLaunch,
    sandbox,
    node: process.version,
    series: sampler.series,
  };
  fs.writeFileSync(path.join(args.out, 'metrics.json'), JSON.stringify(metrics, null, 2));
  const summary = { ...metrics, series: undefined, sandbox: undefined };
  process.stdout.write(`${JSON.stringify(summary)}\n`);
}

main().then(
  () => process.exit(0),
  (err) => {
    process.stderr.write(`run-site: ${(err as Error)?.stack || err}\n`);
    process.exit(1);
  },
);
