// Deterministic stub fixtures only. This is not a model-quality benchmark.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as prettier from 'prettier';
import { normalizeCatalogOutput } from '../../../shared/ai/catalog';
import { promptDigest } from '../../ai-eval/score';
import { scoreCase, type GroundTruth } from '../score';

const here = path.dirname(fileURLToPath(import.meta.url));
const cases = JSON.parse(fs.readFileSync(path.join(here, '../cases.json'), 'utf8')) as {
  posts: {
    id: string;
    mediaType: string;
    thumbnailPath?: string;
    imagePath?: string;
    media?: { type?: string; localPath?: string }[];
  }[];
};
const gold = JSON.parse(fs.readFileSync(path.join(here, '../ground-truth.json'), 'utf8')) as Record<
  string,
  GroundTruth
>;
const fixtures = cases.posts.map((post, index) => {
  const gt = gold[String(post.id)];
  const tags = [
    ...new Set(
      [gt.subject, ...(gt.mustHave ?? []), ...(gt.acceptable ?? [])].filter(
        (tag): tag is string => !!tag,
      ),
    ),
  ];
  const answer = {
    description: gt.subject ?? 'Synthetic catalog subject',
    general_tags: tags.slice(0, 3),
    specific_tags: tags.slice(3, 10),
    entities: [],
    search_keywords: (gt.idealKeywords ?? []).slice(0, 6),
    save_reason: 'Synthetic reference for pipeline verification',
    language: 'it',
  };
  const declared =
    (post.thumbnailPath ? 1 : 0) +
    (post.media ?? []).filter((m) => m.type !== 'video' && m.localPath).length +
    (post.imagePath && !post.media?.length ? 1 : 0);
  return {
    id: String(post.id),
    mediaType: post.mediaType,
    caption: `Synthetic pipeline case ${index + 1}. #tool #type #art #design #study`,
    sourceImages: post.mediaType === 'text' ? 0 : Math.max(1, declared),
    images: post.mediaType === 'text' ? 0 : Math.max(1, Math.min(6, declared)),
    answer,
  };
});
const raw = fixtures.map((f) => {
  const normalized = normalizeCatalogOutput(f.answer, 'social');
  return {
    id: f.id,
    mediaType: f.mediaType,
    caption: f.caption.slice(0, 120),
    tags: normalized.tags,
    keywords: normalized.keywords,
    entities: normalized.entities,
    description: normalized.description,
    error: null,
  };
});
const composite = raw.reduce((sum, r) => sum + scoreCase(gold[r.id], r).composite, 0) / raw.length;
for (const [filename, data] of Object.entries({
  'canned.json': fixtures,
  'expected-raw.json': raw,
  'contract.json': {
    digest: promptDigest('catalog'),
    cases: raw.length,
    composite,
    profiles: ['poster-480', 'poster-1024'],
    source: 'synthetic CAS and gold-derived canned answers; no live inference',
    realRunReport: 'docs/web-port/reports/x1-node-benchmark-2026-10-03.md',
  },
})) {
  const target = path.join(here, filename);
  const config = (await prettier.resolveConfig(target)) ?? {};
  const text = await prettier.format(JSON.stringify(data, null, 2), {
    ...config,
    filepath: target,
  });
  if (process.argv.includes('--check')) {
    if (!fs.existsSync(target) || fs.readFileSync(target, 'utf8') !== text)
      throw new Error(`stale web eval fixture: ${filename}`);
  } else fs.writeFileSync(target, text);
}
