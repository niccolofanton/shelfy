// Scores a run of catalog answers against a gold file, field by field (shared/ai/score.ts),
// for tuning the prompts and schemas of shared/ai/ (owner task X1) and comparing runs.
//
//   pnpm exec tsx scripts/ai-eval/score.ts --gold=<gold.json> --answers=<answers.json>
//                                          [--json=<report.json>] [--worst=<n>]
//
// gold.json: { "posts": { "<id>": { "kind", "mediaType"?, "catalog": {...} } } }
// answers.json: { "<id>": <the model's answer: JSON object, raw text, or null> }
//
// The report starts with the digest of the catalog prompts and schemas the run should have
// used, so a score names its prompt version. Real gold and answers are owner data: keep them
// outside the repo (../shelfy-web-local/ref/), never commit them. The fixtures under
// shared/ai/fixtures/ show the formats:
//
//   pnpm exec tsx scripts/ai-eval/score.ts --gold=shared/ai/fixtures/gold.json \
//     --answers=shared/ai/fixtures/answers.json

import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { file, task, type TaskName } from '../../shared/ai/prompts';
import { FIELDS, scoreRun, type GoldFile, type ScoreReport } from '../../shared/ai/score';

/** sha256 (12 hex digits) of a task's manifest entry and the files it names. */
export function promptDigest(name: TaskName): string {
  const spec = task(name);
  const hash = createHash('sha256').update(JSON.stringify(spec));
  for (const f of [spec.system, spec.user, spec.schema?.file]) {
    if (f) hash.update(`\0${f}\0${file(f)}`);
  }
  return hash.digest('hex').slice(0, 12);
}

/** The report as text. */
export function formatReport(report: ScoreReport, worst = 10): string {
  const lines: string[] = [];
  lines.push(
    `prompts: catalog ${promptDigest('catalog')} · web_catalog ${promptDigest('web_catalog')}`,
  );
  const c = report.counts;
  lines.push(
    `posts ${c.posts} · answered ${c.answered} · missing ${c.missing} · invalid ${c.invalid}`,
  );
  lines.push('');
  for (const field of FIELDS) {
    const mean = report.fields[field];
    if (mean !== undefined) lines.push(`${field.padEnd(16)} ${mean.toFixed(3)}`);
  }
  lines.push(`${'composite'.padEnd(16)} ${report.composite.toFixed(3)}`);
  const types = Object.entries(report.byMediaType);
  if (types.length) {
    lines.push('');
    for (const [type, t] of types) {
      lines.push(`${type.padEnd(16)} ${t.composite.toFixed(3)}  (${t.posts} posts)`);
    }
  }
  const sorted = [...report.posts].sort((a, b) => a.composite - b.composite).slice(0, worst);
  if (sorted.length) {
    lines.push('', `worst ${sorted.length}:`);
    for (const p of sorted) {
      const detail =
        p.status === 'answered'
          ? (['specific_tags', 'general_tags'] as const)
              .map((f) => {
                const s = p.fields[f];
                return s?.missed?.length ? `${f} missed ${s.missed.join(', ')}` : '';
              })
              .filter(Boolean)
              .join('; ')
          : `${p.status}${p.problem ? `: ${p.problem}` : ''}`;
      lines.push(
        `  ${p.composite.toFixed(3)}  ${p.id}${p.mediaType ? ` (${p.mediaType})` : ''}  ${detail}`,
      );
    }
  }
  return lines.join('\n');
}

function arg(name: string): string | undefined {
  const prefix = `--${name}=`;
  return process.argv.find((a) => a.startsWith(prefix))?.slice(prefix.length);
}

function main(): number {
  const goldPath = arg('gold');
  const answersPath = arg('answers');
  if (!goldPath || !answersPath) {
    console.error('usage: score.ts --gold=<gold.json> --answers=<answers.json> [--json=<out>]');
    return 2;
  }
  const gold = JSON.parse(fs.readFileSync(goldPath, 'utf8')) as GoldFile;
  const answers = JSON.parse(fs.readFileSync(answersPath, 'utf8')) as Record<string, unknown>;
  const report = scoreRun(gold, answers);
  console.log(formatReport(report, Number(arg('worst') ?? 10)));
  const out = arg('json');
  if (out) {
    fs.writeFileSync(out, JSON.stringify(report, null, 2) + '\n');
    console.log(`\nwrote ${path.relative(process.cwd(), out)}`);
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
