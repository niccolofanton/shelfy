// UX-0 (audit §3.7): the two UI languages stay in step. Every key of every
// message namespace exists in both, plurals have the same shape, `{vars}`
// match, and an Italian value that equals its English one must be on the
// allowlist below (brand names, loanwords). Known copy leaks of the audit are
// listed in KNOWN_LEAKS: the test reports them (it.fails-style) and fails the
// day one is fixed, so the entry is removed with the fix.
import { describe, expect, it } from 'vitest';

type Value = string | { one?: string; other?: string };
type Namespace = { it?: Record<string, Value>; en?: Record<string, Value> };

const modules = import.meta.glob('../src/i18n/messages/*.ts', {
  eager: true,
  import: 'default',
}) as Record<string, Namespace>;

const namespaces = Object.entries(modules)
  .map(([path, mod]) => ({ name: path.replace(/^.*\/(\w+)\.ts$/, '$1'), mod }))
  .sort((a, b) => a.name.localeCompare(b.name));

// Italian equal to English on purpose: brand names, proper nouns and words
// the Italian UI uses as they are. Matched on the whole value, case-insensitive.
const SAME_VALUE_ALLOWED = new Set(
  [
    'Shelfy',
    'Instagram',
    'X',
    'Pinterest',
    'Post',
    'Account',
    'AI',
    'Email',
    'Online',
    'Offline',
    'Passkey',
    'Download',
    'Chrome',
    'Safari',
    'OK',
    'Link',
    'Tag',
    'Web',
    'Sync',
    'Copyright.',
    'Privacy / GDPR.',
    'Server: {host}',
    'CF-Access-Client-Id',
    'CF-Access-Client-Secret',
    'Downloads',
    'Feedback',
    'Menu',
    'shader GLSL raymarching',
    'generative art TouchDesigner',
    'Hetzner',
    'JSON',
    'ZIP',
    'URL',
    'PDF',
    'API',
    'ID',
    // Technical terms and cognates the Italian UI keeps as they are.
    'CPU',
    'RAM',
    'GPU',
    'Beta',
    'Video',
    'Chat',
    'Design',
    'Media',
    'Database',
    'Sitemap',
    'Palette',
    'Portfolio',
    'E-commerce',
    'SaaS',
    'App',
    'Blog',
    'Web app',
    'Corporate',
    'Layout',
    'Hero',
    'Landing page',
    'Directory',
    'Fintech',
    'Gaming',
    'Automotive',
    'Beauty',
    'AAA',
    'AA',
    'Output',
    'Screenshot',
    'Micro-batch',
    'Sync',
    'auto',
    'video',
    'firewall',
    'Food & beverage',
    'Crypto / Web3',
    'NVIDIA (CUDA)',
    'AMD/Intel (Vulkan)',
    'Apple (Metal)',
    '{n} GB RAM',
    '{n} GB',
    'Auto-tag {count}/{total}',
    'base {px}px',
    '{page} · Shelfy',
    'Download {count}/{total}',
    'Sync {platform}',
    'Tags Explorer',
  ].map((v) => v.toLowerCase()),
);

// Values that carry no words to translate: numbers, symbols, a lone
// placeholder, a URL or an e-mail address.
function isLanguageNeutral(value: string): boolean {
  const stripped = value.replace(/\{\w+\}/g, '').trim();
  return !/\p{L}{2,}/u.test(stripped) || /^https?:\/\//.test(stripped) || stripped.includes('@');
}

// Namespaces whose values are design and web-tech vocabulary the Italian UI
// keeps in English (Masonry, Glassmorphism, CMS, Fontshare…).
const SAME_NAMESPACES = new Set(['webVocab']);

// Audit §3.7 known leaks (UX lanes fix them; remove an entry with its fix).
// `namespace.key`: the Italian text is still the English one.
const KNOWN_LEAKS = new Set<string>([
  'browser.autoImport',
  'importFolder.boardFallback',
  'postCard.manualBookmark',
  'postCard.unknownAuthor',
  'sidebar.aiqueue',
  'filterDrawer.bookmarks',
  'filterDrawer.mediaAll',
  'filterDrawer.downloadAll',
  'filterDrawer.aiAll',
  'filterDrawer.mediaFile',
  'postModal.manualBookmark',
  'postModal.unknownAuthor',
  'postModal.assetThumbnail',
  'postModal.mediaFile',
  'postModal.home',
  'postModal.award',
  'postModal.platformX',
  'settings.typeThumbnailLabel',
  'settings.typeImageLabel',
  'settings.typeVideoLabel',
  'settings.remoteBaseUrl',
  'settings.dataTitle',
  'settings.versionPillWeb',
  'activity.shortAnalysis',
  'activity.shortDownload',
  'aiQueue.heading',
  'aiQueue.phaseProcessing',
  'aiQueue.phaseTags',
  'aiQueue.platformWeb',
  'aiTags.title',
  'aiTags.coverage',
  'aiSearch.stop',
  'aiSearch.sourceSocial',
  'aiWebsites.headerTitle',
  'aiWebsites.pageFooter',
  'aiWebsites.sectionPalette',
  'aiWebsites.captureViewport',
  'gallery.viewCanvas',
]);

function strings(value: Value): string[] {
  return typeof value === 'string' ? [value] : [value.one ?? '', value.other ?? ''];
}

function vars(value: string): string {
  return [...value.matchAll(/\{(\w+)\}/g)]
    .map((m) => m[1])
    .sort()
    .join(',');
}

describe('i18n parity', () => {
  it('finds the namespaces', () => {
    expect(namespaces.length).toBeGreaterThan(20);
  });

  for (const { name, mod } of namespaces) {
    describe(name, () => {
      const it_ = mod.it ?? {};
      const en = mod.en ?? {};

      it('has the same keys in Italian and English', () => {
        const onlyIt = Object.keys(it_).filter((k) => !(k in en));
        const onlyEn = Object.keys(en).filter((k) => !(k in it_));
        expect({ onlyIt, onlyEn }).toEqual({ onlyIt: [], onlyEn: [] });
      });

      it('has the same value shape and {variables} in both', () => {
        const mismatches: string[] = [];
        for (const key of Object.keys(en)) {
          const a = it_[key];
          const b = en[key];
          if (a === undefined) continue;
          if (typeof a !== typeof b) {
            mismatches.push(`${key}: plural shape differs`);
            continue;
          }
          const sa = strings(a);
          const sb = strings(b);
          sa.forEach((s, i) => {
            if (vars(s) !== vars(sb[i])) mismatches.push(`${key}: {variables} differ`);
          });
        }
        expect(mismatches).toEqual([]);
      });

      it('has no empty values', () => {
        const empty = [...Object.entries(it_), ...Object.entries(en)]
          .filter(([, v]) => strings(v).every((s) => s.trim() === ''))
          .map(([k]) => k);
        expect(empty).toEqual([]);
      });

      it('does not leave Italian equal to English off the allowlist', () => {
        const leaks: string[] = [];
        for (const key of Object.keys(en)) {
          const a = it_[key];
          if (a === undefined) continue;
          const sa = strings(a);
          const sb = strings(en[key]);
          const same = sa.every((s, i) => s === sb[i]) && sa.some((s) => s.trim() !== '');
          if (!same) continue;
          if (
            sa.every((s) => isLanguageNeutral(s) || SAME_VALUE_ALLOWED.has(s.trim().toLowerCase()))
          ) {
            continue;
          }
          if (SAME_NAMESPACES.has(name) || KNOWN_LEAKS.has(`${name}.${key}`)) continue;
          leaks.push(`${name}.${key} = ${JSON.stringify(sa[0])}`);
        }
        expect(leaks).toEqual([]);
      });
    });
  }

  it('lists no known leak that is already fixed', () => {
    const stale = [...KNOWN_LEAKS].filter((id) => {
      const [ns, ...rest] = id.split('.');
      const key = rest.join('.');
      const mod = namespaces.find((n) => n.name === ns)?.mod;
      const a = mod?.it?.[key];
      const b = mod?.en?.[key];
      return a === undefined || JSON.stringify(a) !== JSON.stringify(b);
    });
    expect(stale).toEqual([]);
  });
});

describe('web manifest', () => {
  it('does not list a platform Shelfy does not support', async () => {
    const { readFileSync } = await import('node:fs');
    const manifest = readFileSync('web/public/manifest.webmanifest', 'utf8');
    expect(manifest).not.toMatch(/tiktok/i);
  });
});
