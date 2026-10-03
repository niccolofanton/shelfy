// Serial operator-node benchmark. Personal inputs/answers stay outside the repo.
// node --env-file=<private.env> --import=tsx scripts/ai-eval/run.ts
//   --input=<bench40> --out=<private-run-dir> --pipeline=baseline|candidate [--limit=8]
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { catalogRequest } from '../../shared/ai/catalog';
import { schemaProblem } from '../../shared/ai/score';
import { promptDigest } from './score';

export interface BenchPost {
  media_type: string;
  caption?: string;
  cover?: string | null;
  media_complete?: boolean;
  slides: {
    type: string;
    file?: string | null;
    poster?: string | null;
    duration_s?: number;
    frames?: { file: string; at_s?: number }[];
  }[];
}

export interface ImageInput {
  file: string;
  atSeconds?: number;
}

function within(root: string, filename: string): boolean {
  const relative = path.relative(root, filename);
  return (
    relative === '' ||
    (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))
  );
}

/** A hook's Git dirs/index/config must not override a child's explicit cwd. */
export function independentGitEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
}

/** Resolve ancestors before creation, including symlinks; never reuse a run directory. */
export function privateOutput(outputArg: string, repoRoots: string[]): string {
  const requested = path.resolve(outputArg);
  if (fs.existsSync(requested) || fs.lstatSync(requested, { throwIfNoEntry: false }))
    throw new Error('choose a fresh output directory for every run');
  let ancestor = path.dirname(requested);
  while (!fs.existsSync(ancestor)) ancestor = path.dirname(ancestor);
  const actual = path.resolve(fs.realpathSync(ancestor), path.relative(ancestor, requested));
  if (repoRoots.some((root) => within(fs.realpathSync(root), actual)))
    throw new Error('outputs must stay outside repo');
  const git = spawnSync('git', ['rev-parse', '--is-inside-work-tree'], {
    cwd: fs.realpathSync(ancestor),
    env: independentGitEnvironment(),
    encoding: 'utf8',
  });
  if (git.status === 0 && git.stdout.trim() === 'true')
    throw new Error('outputs must stay outside every Git worktree');
  fs.mkdirSync(path.dirname(actual), { recursive: true, mode: 0o700 });
  fs.mkdirSync(actual, { mode: 0o700 });
  return actual;
}

function localFile(directory: string, name: string): string | undefined {
  const root = fs.realpathSync(directory);
  const filename = path.resolve(root, name);
  if (!within(root, filename)) throw new Error('benchmark input escapes its post directory');
  if (!fs.existsSync(filename) || !fs.statSync(filename).isFile()) return undefined;
  const actual = fs.realpathSync(filename);
  if (!within(root, actual)) throw new Error('benchmark input escapes its post directory');
  return actual;
}

function extractedFrames(
  post: BenchPost,
  slide: BenchPost['slides'][number],
  directory: string,
): string[] {
  const slideIndex = post.slides.indexOf(slide);
  const folder = slide.file
    ? path.join(path.dirname(slide.file), path.basename(slide.file, path.extname(slide.file)))
    : `slide-${slideIndex}`;
  return (slide.frames ?? []).map((frame) => {
    const resolved =
      localFile(directory, frame.file) ?? localFile(directory, path.join(folder, frame.file));
    if (!resolved) throw new Error('declared benchmark frame is missing');
    return resolved;
  });
}

/** Keep first/last frames and sample the rest evenly. */
export function spread<T>(items: T[], max: number): T[] {
  if (items.length <= max) return items;
  if (max === 1) return [items[0]];
  return Array.from(
    { length: max },
    (_, n) => items[Math.round((n * (items.length - 1)) / (max - 1))],
  );
}

/** Only local existing files within a post directory; never fetch social URLs. */
export function imagePaths(post: BenchPost, directory: string): string[] {
  const names: string[] = [];
  for (const slide of post.slides) {
    if (slide.type === 'video') {
      if (post.media_type === 'video') {
        if (!slide.frames?.length)
          throw new Error('complete benchmark video has no extracted frames');
        names.push(...spread(extractedFrames(post, slide, directory), 4));
      }
    } else if (slide.file) names.push(slide.file);
  }
  if (!names.length && post.cover) names.push(post.cover);
  const seen = new Set<string>();
  const files: string[] = [];
  for (const name of names) {
    const actual = localFile(directory, name);
    if (!actual) throw new Error('declared benchmark image is missing');
    const digest = createHash('sha256').update(fs.readFileSync(actual)).digest('hex');
    if (seen.has(digest)) continue;
    seen.add(digest);
    files.push(actual);
  }
  if (post.media_type === 'video' && !files.length)
    throw new Error('complete benchmark video has no media');
  return spread(files, post.media_type === 'video' ? 4 : 8);
}

/** P3-13 deep ordering: cover, slides (max 8), poster, four midpoint keyframes; max 6 JPEGs. */
export function candidateInputs(
  post: BenchPost,
  directory: string,
  media: 'poster' | 'deep' = 'deep',
): ImageInput[] {
  const inputs: ImageInput[] = [];
  const seen = new Set<string>();
  const still = (name: string): void => {
    const file = localFile(directory, name);
    if (!file) throw new Error('declared benchmark image is missing');
    const digest = createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    if (seen.has(digest)) return;
    seen.add(digest);
    inputs.push({ file });
  };
  if (post.cover) still(post.cover);
  for (const slide of post.slides.slice(0, 8)) {
    if (inputs.length >= 6) break;
    if (slide.type !== 'video') {
      if (slide.file) still(slide.file);
      continue;
    }
    // Production poster mode has no video object: never synthesize a still or fetch video.
    if (media === 'poster') {
      if (slide.poster) still(slide.poster);
      continue;
    }
    const video = slide.file ? localFile(directory, slide.file) : undefined;
    if (slide.file && !video) throw new Error('declared benchmark video is missing');
    if (slide.poster) still(slide.poster);
    else if (video)
      inputs.push({ file: video, atSeconds: Math.min((slide.duration_s ?? 0) * 0.1, 1) });
    const count = Math.min(4, 6 - inputs.length);
    if (video) {
      const duration = slide.duration_s ?? 0;
      for (let n = 0; n < (duration > 0 ? count : Math.min(count, 1)); n++)
        inputs.push({
          file: video,
          atSeconds: duration > 0 ? (duration * (2 * n + 1)) / (2 * count) : 0,
        });
    } else {
      const frames = spread(extractedFrames(post, slide, directory), count);
      inputs.push(...frames.map((file) => ({ file })));
    }
  }
  if (post.media_type === 'video' && !inputs.length)
    throw new Error('complete benchmark video has no media');
  return inputs.slice(0, 6);
}

export function trimHashtags(caption: string): string {
  const tail = /(?:\s*#\S+){5,}\s*$/u;
  return caption.replace(tail, '').trimEnd();
}

/** Same shared builder as the desktop; keep argument order covered by a test. */
export function benchRequest(
  post: BenchPost,
  hasImages: boolean,
  candidate: boolean,
  hashtags: 'trim' | 'weak' = 'weak',
) {
  return catalogRequest(
    candidate && hashtags === 'trim' ? trimHashtags(post.caption ?? '') : post.caption,
    [],
    hasImages,
    'social',
  );
}

function arg(name: string): string | undefined {
  return process.argv.find((a) => a.startsWith(`--${name}=`))?.slice(name.length + 3);
}

function writePrivate(filename: string, data: unknown): void {
  fs.writeFileSync(filename, JSON.stringify(data, null, 2) + '\n', { mode: 0o600 });
  fs.chmodSync(filename, 0o600);
}

function jpeg(input: ImageInput, size: number): string {
  const result = spawnSync(
    process.env.FFMPEG_BIN ?? '/opt/homebrew/bin/ffmpeg',
    [
      '-v',
      'error',
      ...(input.atSeconds === undefined ? [] : ['-ss', String(input.atSeconds)]),
      '-i',
      input.file,
      '-vf',
      `scale=w='min(${size},iw)':h='min(${size},ih)':force_original_aspect_ratio=decrease`,
      '-frames:v',
      '1',
      '-q:v',
      '3',
      '-f',
      'image2pipe',
      '-vcodec',
      'mjpeg',
      'pipe:1',
    ],
    { maxBuffer: 16 * 1024 * 1024, timeout: 30_000 },
  );
  if (result.status !== 0 || !result.stdout.length) throw new Error('jpeg conversion failed');
  return `data:image/jpeg;base64,${result.stdout.toString('base64')}`;
}

async function main(): Promise<void> {
  const inputArg = arg('input');
  const outputArg = arg('out');
  const pipeline = arg('pipeline');
  const media = arg('media') ?? (pipeline === 'candidate' ? 'poster' : 'deep');
  const hashtags = arg('hashtags') ?? 'weak';
  if (!['trim', 'weak'].includes(hashtags)) throw new Error('hashtags must be trim or weak');
  if (!['poster', 'deep'].includes(media) || (pipeline === 'baseline' && media !== 'deep'))
    throw new Error('media must be deep, or poster for the candidate pipeline');
  if (!inputArg || !outputArg || !['baseline', 'candidate'].includes(pipeline ?? '')) {
    throw new Error(
      'required: --input=<bench40> --out=<private-dir> --pipeline=baseline|candidate',
    );
  }
  const input = fs.realpathSync(inputArg);
  const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
  const baseUrl = process.env.SHELFY_EVAL_ORNITH_BASE_URL;
  const apiKey = process.env.SHELFY_EVAL_ORNITH_API_KEY;
  const model = process.env.SHELFY_EVAL_ORNITH_VISION_MODEL;
  if (!baseUrl || !apiKey || !model) throw new Error('private operator eval config is missing');
  const origin = new URL(baseUrl);
  if (!['http:', 'https:'].includes(origin.protocol) || origin.username || origin.password) {
    throw new Error('invalid operator origin');
  }
  const endpoint = `${baseUrl.replace(/\/$/, '').replace(/\/v1$/, '')}/v1/chat/completions`;
  const limit = Number(arg('limit') ?? 40);
  if (!Number.isInteger(limit) || limit < 1 || limit > 40) throw new Error('limit must be 1..40');
  const all = fs
    .readdirSync(input)
    .filter((n) => /^\d{2}$/.test(n))
    .sort()
    .map((id) => ({
      id,
      post: JSON.parse(fs.readFileSync(path.join(input, id, 'post.json'), 'utf8')) as BenchPost,
    }));
  // A small comparison is stratified; a full run includes the gated posts in its counts.
  const sample =
    limit === 40
      ? all
      : [
          ...all
            .filter((x) => x.post.media_type === 'image' && x.post.media_complete !== false)
            .slice(0, Math.max(1, Math.floor(limit / 4))),
          ...all
            .filter((x) => x.post.media_type === 'carousel' && x.post.media_complete !== false)
            .slice(0, Math.max(1, Math.floor(limit / 4))),
          ...all.filter((x) => x.post.media_type === 'video' && x.post.media_complete !== false),
        ].slice(0, limit);
  // Fail the entire preflight before any model call if a declared input is missing.
  const prepared = sample.map(({ id, post }) => ({
    id,
    post,
    inputs:
      post.media_complete === false
        ? []
        : pipeline === 'candidate'
          ? candidateInputs(post, path.join(input, id), media as 'poster' | 'deep')
          : imagePaths(post, path.join(input, id)).map((file) => ({ file })),
  }));
  const typeCounts = Object.fromEntries(
    ['image', 'carousel', 'video'].map((type) => [
      type,
      prepared.filter(({ post }) => post.media_type === type).length,
    ]),
  );
  console.log(
    `preflight: ${sample.length} posts; types ${JSON.stringify(typeCounts)}; image counts ${prepared.map(({ inputs }) => inputs.length).join(',')}`,
  );
  // Decode every selected source before the first provider request as well.
  const encoded = prepared.map(({ inputs }) =>
    inputs.map((source) => jpeg(source, pipeline === 'candidate' ? 1024 : 448)),
  );
  const commonGit = spawnSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], {
    cwd: repo,
    env: independentGitEnvironment(),
    encoding: 'utf8',
  });
  if (commonGit.status !== 0) throw new Error('cannot establish repository privacy boundary');
  const output = privateOutput(outputArg, [repo, path.dirname(commonGit.stdout.trim())]);
  const manifest = {
    pipeline,
    media,
    captionPolicy: hashtags,
    model,
    digest: promptDigest('catalog'),
    goldDigest: fs.existsSync(path.join(input, 'gold.json'))
      ? createHash('sha256')
          .update(fs.readFileSync(path.join(input, 'gold.json')))
          .digest('hex')
      : null,
    count: sample.length,
    maxImages: pipeline === 'candidate' ? 6 : { video: 4, image: 8, carousel: 8 },
    maxSlides: pipeline === 'candidate' ? 8 : null,
    videoSelection:
      media === 'poster'
        ? 'existing cover/image slides/exported posters only; no video decoding'
        : pipeline === 'candidate'
          ? 'poster <=1s + up to 4 equal-span midpoint keyframes'
          : 'up to 4 spread extracted frames',
    source: 'private independent benchmark extraction; not production media database',
    typeCounts,
    imageCounts: prepared.map(({ inputs }) => inputs.length),
    maxEdge: pipeline === 'candidate' ? 1024 : 448,
    serial: true,
    startedAt: new Date().toISOString(),
  };
  const manifestPath = path.join(output, 'run.json');
  if (fs.existsSync(manifestPath)) throw new Error('choose a fresh output directory for every run');
  writePrivate(manifestPath, manifest);
  const answers: Record<string, unknown> = {};
  const timings: Record<string, unknown> = {};
  for (const [index, { id, post }] of prepared.entries()) {
    const started = performance.now();
    try {
      if (post.media_complete === false) {
        answers[id] = null;
        timings[id] = { status: 'gated', elapsedMs: 0 };
        continue;
      }
      const candidate = pipeline === 'candidate';
      const images = encoded[index];
      const request = benchRequest(post, images.length > 0, candidate, hashtags as 'trim' | 'weak');
      const response = await fetch(endpoint, {
        method: 'POST',
        headers: { Authorization: `Bearer ${apiKey}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          model,
          temperature: request.temperature,
          max_tokens: request.maxTokens,
          chat_template_kwargs: { enable_thinking: false },
          messages: [
            { role: 'system', content: request.system },
            {
              role: 'user',
              content: [
                { type: 'text', text: request.user },
                ...images.map((url) => ({ type: 'image_url', image_url: { url } })),
              ],
            },
          ],
          response_format: {
            type: 'json_schema',
            json_schema: { name: request.schema.name, strict: true, schema: request.schema.schema },
          },
        }),
        signal: AbortSignal.timeout(240_000),
      });
      if (!response.ok) throw new Error(`provider HTTP ${response.status}`);
      const result = (await response.json()) as {
        choices?: { message?: { content?: string }; finish_reason?: string }[];
        usage?: unknown;
      };
      const raw = result.choices?.[0]?.message?.content;
      if (!raw) throw new Error('provider returned no catalog');
      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        parsed = raw;
      }
      answers[id] = parsed;
      timings[id] = {
        status:
          typeof parsed === 'string' || schemaProblem(request.schema.schema, parsed)
            ? 'invalid'
            : 'answered',
        elapsedMs: Math.round(performance.now() - started),
        imageCount: images.length,
        finishReason: result.choices?.[0]?.finish_reason,
        usage: result.usage,
      };
    } catch (error) {
      answers[id] = null;
      timings[id] = {
        status: 'error',
        elapsedMs: Math.round(performance.now() - started),
        error:
          error instanceof Error && /^provider HTTP \d+$/.test(error.message)
            ? error.message
            : 'request or preprocessing failed',
      };
    } finally {
      writePrivate(path.join(output, 'answers.json'), answers);
      writePrivate(path.join(output, 'timings.json'), timings);
      console.log(
        `benchmark ${index + 1}/${sample.length}: ${(timings[id] as { status: string }).status}`,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  const counts = Object.fromEntries(
    ['answered', 'invalid', 'error', 'gated'].map((status) => [
      status,
      Object.values(timings).filter((entry) => (entry as { status: string }).status === status)
        .length,
    ]),
  );
  writePrivate(manifestPath, {
    ...manifest,
    counts,
    schemaValid: counts.invalid === 0 && counts.error === 0,
    completedAt: new Date().toISOString(),
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(() => {
    console.error('benchmark failed; check private inputs/config and use a fresh output directory');
    process.exitCode = 1;
  });
}
