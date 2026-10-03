// Pure site assembly for the web-reference capture.
//
// Given a SiteCapture (what the browser produced) and the fetched og:image /
// favicon, derive the design metadata the catalog and the UI consume: palette,
// typography, tech, awards, traits, per-page digests, the site meta object and
// the cover choice. It is DETERMINISTIC and path-free (asset bytes are read from
// disk for the palette only), so the desktop (weborchestrator.captureWebReference)
// and the P4 capture service produce the SAME assembly from the same capture —
// the parity the service's manifest is tested against.
//
// This module owns pickIcon (the best-raster-icon chooser), which both callers
// use to pick the favicon URL before fetching it.

import * as meta from './metadata';
import type { SiteCapture, CapturedPage } from './capture';

// ─── Output shape ─────────────────────────────────────────────────────────────

export interface AssemblySection {
  kind: string;
  heading: string;
  top: number;
  cssHeight: number;
}

// Per-page design metadata (path-free: asset files are referenced by the caller
// from the CapturedPage, by index into SiteAssembly.pages == SiteCapture.pages).
export interface AssemblyPage {
  url: string;
  requestedUrl: string;
  pageType: string;
  title: string;
  status: number | null;
  heightCss: number;
  capped: boolean;
  jacked: boolean;
  qc: { status: string; reason: string };
  ogImage: string;
  contentText: string;
  digest: { h1: string; headings: string[]; ctas: string[] };
  sections: AssemblySection[];
}

// The site meta object (web_meta_json), WITHOUT local file paths: the caller adds
// ogImagePath / favicon / video, which differ between the desktop (absolute paths)
// and the service (role-keyed file names in the manifest).
export interface SiteMeta {
  schema: number;
  siteName: string;
  title: string;
  description: string;
  lang?: string;
  languages: string[];
  ogImage: string;
  themeColor: string | null;
  canonical: string | null;
  rss: string | null;
  jsonldTypes: string[];
  organization: ReturnType<typeof meta.parseJsonLd>['organization'];
  social: { platform: string; href: string }[];
  credits: { text: string; href: string }[];
  scheme: string | null;
  contrast: meta.PaletteResult['contrast'];
  typeScale: meta.TypographyResult['scale'];
  baseSize: number | null;
  scaleRatio: number | null;
  tech: meta.TechInfo[];
  traits: meta.SiteTraits;
  pageCount: number;
  awardTags: string[];
  awardEntities: string[];
  capture: {
    engine: string;
    userAgent: string;
    viewport: { width: number; height: number; scale: number };
    discovery: string;
    skipped: { url: string; reason: string }[];
    consent: { cmp: string | null; result: string | null };
    timings: Record<string, number>;
  };
  singlePage?: boolean;
}

export interface SiteAssembly {
  title: string;
  siteName: string;
  description: string;
  lang: string | null;
  languages: string[];
  palette: meta.PaletteSwatch[];
  scheme: string | null;
  contrast: meta.PaletteResult['contrast'];
  fonts: meta.FontInfo[];
  techNames: string[];
  awards: meta.AwardInfo[];
  pages: AssemblyPage[];
  digests: meta.PageDigest[];
  siteText: string;
  // The page-0 cover choice: the hero (or band0) unless the hero is poor and an
  // og:image was fetched, in which case the og:image. Path-free: `og` means "use
  // the caller's fetched og:image", `hero`/`band0` mean "use that page-0 asset".
  cover: 'hero' | 'band0' | 'og';
  meta: SiteMeta;
}

export interface AssembleOptions {
  ogImageUrl?: string | null; // the remote og:image URL (for meta.ogImage)
  ogFetched?: boolean; // whether the caller fetched an og:image (drives the cover)
  singlePage?: boolean;
  signal?: AbortSignal;
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

interface ProbeHead {
  title?: string;
  lang?: string;
  description?: string;
  ogTitle?: string;
  ogDescription?: string;
  ogImage?: string;
  ogSiteName?: string;
  applicationName?: string;
  themeColor?: string;
  canonical?: string;
  rss?: string;
  icons?: { href: string; rel: string; sizes: string; type: string }[];
  hreflang?: { lang?: string; href?: string }[];
  jsonld?: string[];
}

interface ProbeExtras {
  head?: ProbeHead;
  social?: { platform: string; href: string }[];
  credits?: { text: string; href: string }[];
}

function webHost(u: string): string {
  try {
    return new URL(u).hostname.toLowerCase().replace(/^www\./, '');
  } catch {
    return '';
  }
}

function titleCase(s: string): string {
  return s.replace(/(^|[\s.-])([a-z])/g, (_, a: string, b: string) => a + b.toUpperCase());
}

// Best raster icon: apple-touch-icon / largest PNG; SVG and ICO as fallbacks.
// Owned here (was private in weborchestrator.ts); used by both capture callers to
// choose the favicon URL before fetching it.
export function pickIcon(
  icons: { href: string; rel: string; sizes: string; type: string }[],
  pageUrl: string,
): string | null {
  const scored = icons
    .filter((i) => i.href && !/^data:/.test(i.href))
    .map((i) => {
      const size = Number((/(\d+)x\d+/.exec(i.sizes || '') || [])[1]) || 0;
      const svg = /svg/.test(i.type) || /\.svg(\?|$)/i.test(i.href);
      const apple = /apple-touch-icon/i.test(i.rel);
      return { href: i.href, score: (apple ? 300 : 0) + Math.min(size, 512) + (svg ? -400 : 0) };
    })
    .sort((a, b) => b.score - a.score);
  if (scored.length) return scored[0].href;
  try {
    return new URL('/favicon.ico', pageUrl).toString();
  } catch {
    return null;
  }
}

// ─── Assembly ──────────────────────────────────────────────────────────────────

export async function assembleSite(
  site: SiteCapture,
  opts: AssembleOptions = {},
): Promise<SiteAssembly> {
  const pages = site.pages;
  const primary = site.primary;
  const finalUrl = primary.url;
  const domain = webHost(finalUrl);
  const head = (primary.probe.head || {}) as ProbeHead;
  const jsonld = meta.parseJsonLd(Array.isArray(head.jsonld) ? head.jsonld : []);
  const siteName =
    head.ogSiteName ||
    jsonld.organization?.name ||
    head.applicationName ||
    titleCase(domain.split('.').slice(0, -1).join('.') || domain);

  const palette = await meta.computePalette(pages, opts.signal).catch(() => null);
  const typo = meta.computeTypography(pages, domain);
  const tech = meta.computeTech(pages);
  const awards = meta.computeAwards(pages, domain, siteName);
  const traits = meta.computeTraits(pages, tech);
  const awardTE = meta.awardTags(awards);
  const digests = pages.map((p, i) => meta.pageDigest(p, i === 0 ? 1600 : 700));
  const probeOf = (p: CapturedPage): ProbeExtras => p.probe as ProbeExtras;

  const title =
    meta.cleanTitle(head.ogTitle || head.title || primary.title || siteName, siteName) || siteName;
  const description =
    head.description || head.ogDescription || jsonld.organization?.description || '';
  const lang = (head.lang || '').split('-')[0].toLowerCase() || null;
  const languages = Array.from(
    new Set(
      (head.hreflang || [])
        .map((h) => String(h.lang || '').toLowerCase())
        .filter((l) => l && l !== 'x-default'),
    ),
  ).slice(0, 20);
  const techNames = tech.filter((t) => t.confidence >= 0.8).map((t) => t.name);

  // Searchable site text: every page's digest (headings, CTAs, readable copy).
  const siteText = digests
    .map((d) =>
      [
        d.h1,
        d.headings.filter((h) => h !== d.h1).join(' · '),
        d.ctas.length ? `CTA: ${d.ctas.join(' · ')}` : '',
        d.text,
      ]
        .filter(Boolean)
        .join('\n'),
    )
    .join('\n\n')
    .slice(0, 12000);

  // Cover: the untouched hero, unless it is a poor frame and an og:image exists.
  const cover: SiteAssembly['cover'] =
    primary.qc.status === 'ok' || !opts.ogFetched ? (primary.hero ? 'hero' : 'band0') : 'og';

  const assemblyPages: AssemblyPage[] = pages.map((p, i) => {
    const d = digests[i];
    return {
      url: p.url,
      requestedUrl: p.requestedUrl,
      pageType: p.pageType,
      title: meta.cleanTitle(p.title, siteName),
      status: p.status,
      heightCss: p.heightCss,
      capped: p.capped,
      jacked: p.jacked,
      qc: p.qc,
      ogImage: String((probeOf(p).head || {}).ogImage || ''),
      contentText: i === 0 ? siteText : d.text,
      digest: { h1: d.h1, headings: d.headings, ctas: d.ctas },
      sections: p.sections.map((s) => ({
        kind: s.kind,
        heading: s.heading,
        top: s.top,
        cssHeight: s.cssHeight,
      })),
    };
  });

  const siteMeta: SiteMeta = {
    schema: 2,
    siteName,
    title,
    description: description.slice(0, 400),
    lang: lang || undefined,
    languages,
    ogImage: opts.ogImageUrl || head.ogImage || '',
    themeColor: head.themeColor || null,
    canonical: head.canonical || null,
    rss: head.rss || null,
    jsonldTypes: jsonld.types,
    organization: jsonld.organization,
    social: probeOf(primary).social || [],
    credits: probeOf(primary).credits || [],
    scheme: palette?.scheme || null,
    contrast: palette?.contrast || null,
    typeScale: typo.scale,
    baseSize: typo.baseSize,
    scaleRatio: typo.ratio,
    tech,
    traits,
    pageCount: pages.length,
    awardTags: awardTE.tags,
    awardEntities: awardTE.entities,
    capture: {
      engine: site.engine,
      userAgent: site.userAgent,
      viewport: { width: 1440, height: 900, scale: 2 },
      discovery: site.discoverySource,
      skipped: site.skipped,
      consent: primary.consent,
      timings: Object.fromEntries(pages.map((p) => [p.url, p.timings.total])),
    },
    ...(opts.singlePage ? { singlePage: true } : {}),
  };

  return {
    title,
    siteName,
    description,
    lang,
    languages,
    palette: palette?.swatches || [],
    scheme: palette?.scheme || null,
    contrast: palette?.contrast || null,
    fonts: typo.fonts,
    techNames,
    awards,
    pages: assemblyPages,
    digests,
    siteText,
    cover,
    meta: siteMeta,
  };
}
