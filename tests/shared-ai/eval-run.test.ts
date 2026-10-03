import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { afterEach, describe, expect, it } from 'vitest';
import {
  benchRequest,
  imagePaths,
  candidateInputs,
  privateOutput,
  spread,
  trimHashtags,
  type BenchPost,
} from '../../scripts/ai-eval/run';

const temporary: string[] = [];
const directory = (): string => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-eval-'));
  temporary.push(dir);
  return dir;
};
afterEach(() => temporary.splice(0).forEach((p) => fs.rmSync(p, { recursive: true, force: true })));

describe('operator benchmark inputs', () => {
  it('passes actual caption and media state to the shared catalog builder', () => {
    const post: BenchPost = { media_type: 'image', caption: 'Synthetic walnut lamp', slides: [] };
    const request = benchRequest(post, true, false);
    expect(request.user).toContain('Synthetic walnut lamp');
    expect(request.user).toContain('<<<CAPTION>>>');
    expect(request.user).not.toContain('This is a text-only post');
    expect(benchRequest(post, false, false).user).toContain('This is a text-only post');
  });

  it('trims a trailing hashtag wall but preserves short captions and internal hashtags', () => {
    expect(trimHashtags('A #lamp study #a #b #c #d #e')).toBe('A #lamp study');
    expect(trimHashtags('A lamp #a #b')).toBe('A lamp #a #b');
    expect(trimHashtags('A #a #b #c #d #e with an explanation')).toContain('explanation');
  });

  it('deduplicates the cover and includes carousel video frames in candidate inputs', () => {
    const dir = directory();
    fs.writeFileSync(path.join(dir, 'cover.jpg'), 'same synthetic bytes');
    fs.writeFileSync(path.join(dir, 'slide.jpg'), 'same synthetic bytes');
    fs.writeFileSync(path.join(dir, 'frame.jpg'), 'different synthetic frame');
    const post: BenchPost = {
      media_type: 'carousel',
      cover: 'cover.jpg',
      slides: [
        { type: 'image', file: 'slide.jpg' },
        { type: 'video', frames: [{ file: 'frame.jpg' }] },
      ],
    };
    expect(candidateInputs(post, dir).map(({ file }) => path.basename(file))).toEqual([
      'cover.jpg',
      'frame.jpg',
    ]);
    expect(imagePaths(post, dir).map((p) => path.basename(p))).toEqual(['slide.jpg']);
  });

  it('refuses path traversal and symlink inputs outside the post directory', () => {
    const outside = directory();
    const dir = path.join(outside, 'post');
    fs.mkdirSync(dir);
    fs.writeFileSync(path.join(outside, 'secret.jpg'), 'outside');
    const post: BenchPost = {
      media_type: 'image',
      slides: [{ type: 'image', file: '../secret.jpg' }],
    };
    expect(() => imagePaths(post, dir)).toThrow(/escapes/);
    expect(() => candidateInputs(post, dir)).toThrow(/escapes/);
    fs.symlinkSync(path.join(outside, 'secret.jpg'), path.join(dir, 'link.jpg'));
    post.slides[0].file = 'link.jpg';
    expect(() => imagePaths(post, dir)).toThrow(/escapes/);
    expect(() => candidateInputs(post, dir)).toThrow(/escapes/);
  });

  it('bounds frames while retaining temporal coverage', () => {
    expect(spread([0, 1, 2, 3, 4, 5], 4)).toEqual([0, 2, 3, 5]);
    expect(spread([0, 1], 4)).toEqual([0, 1]);
    expect(spread([0, 1], 1)).toEqual([0]);
  });

  it('resolves the real extraction layout instead of silently sending caption-only video', () => {
    const dir = directory();
    fs.mkdirSync(path.join(dir, 'slide-0'));
    for (let n = 1; n <= 8; n++)
      fs.writeFileSync(path.join(dir, 'slide-0', `frame-${n}.jpg`), `synthetic frame ${n}`);
    const post: BenchPost = {
      media_type: 'video',
      slides: [
        {
          type: 'video',
          file: 'slide-0.mp4',
          frames: Array.from({ length: 8 }, (_, n) => ({ file: `frame-${n + 1}.jpg` })),
        },
      ],
    };
    expect(imagePaths(post, dir).map((file) => path.basename(file))).toEqual([
      'frame-1.jpg',
      'frame-3.jpg',
      'frame-6.jpg',
      'frame-8.jpg',
    ]);
    fs.unlinkSync(path.join(dir, 'slide-0', 'frame-4.jpg'));
    expect(() => imagePaths(post, dir)).toThrow(/frame is missing/);
    expect(() => imagePaths({ media_type: 'video', slides: [] }, dir)).toThrow(/no media/);
  });

  it('keeps P3-13 deep cover/poster/keyframe order and its six-image cap', () => {
    const dir = directory();
    fs.writeFileSync(path.join(dir, 'cover.jpg'), 'synthetic title card');
    fs.writeFileSync(path.join(dir, 'slide-0.mp4'), 'synthetic video');
    const post: BenchPost = {
      media_type: 'video',
      cover: 'cover.jpg',
      slides: [{ type: 'video', file: 'slide-0.mp4', duration_s: 8 }],
    };
    expect(
      candidateInputs(post, dir).map(({ file, atSeconds }) => [path.basename(file), atSeconds]),
    ).toEqual([
      ['cover.jpg', undefined],
      ['slide-0.mp4', 0.8],
      ['slide-0.mp4', 1],
      ['slide-0.mp4', 3],
      ['slide-0.mp4', 5],
      ['slide-0.mp4', 7],
    ]);
    post.slides.push({ type: 'image', file: 'cover.jpg' });
    expect(candidateInputs(post, dir)).toHaveLength(6);
  });

  it('rejects repo outputs through symlink ancestors and refuses existing output directories', () => {
    const root = directory();
    const repo = path.join(root, 'repo');
    fs.mkdirSync(repo);
    fs.symlinkSync(repo, path.join(root, 'alias'));
    expect(() => privateOutput(path.join(root, 'alias', 'runs', 'new'), [repo])).toThrow(
      /outside repo/,
    );
    const output = privateOutput(path.join(root, 'private', 'run'), [repo]);
    expect(fs.statSync(output).mode & 0o777).toBe(0o700);
    expect(() => privateOutput(output, [repo])).toThrow(/fresh output/);
  });

  it('matches engine hashtag trimming including punctuation and preserves leading whitespace', () => {
    expect(trimHashtags('  A lamp #a.b #b-c #c! #d #e')).toBe('  A lamp');
    expect(trimHashtags('#a #b #c #d #e')).toBe('');
  });

  it('poster profile never decodes an MP4 or substitutes an extracted video frame', () => {
    const dir = directory();
    fs.writeFileSync(path.join(dir, 'cover.jpg'), 'synthetic title card');
    const post: BenchPost = {
      media_type: 'video',
      cover: 'cover.jpg',
      slides: [{ type: 'video', file: 'not-present.mp4', frames: [{ file: 'not-present.jpg' }] }],
    };
    expect(candidateInputs(post, dir, 'poster')).toEqual([
      { file: fs.realpathSync(path.join(dir, 'cover.jpg')) },
    ]);
    expect(() => candidateInputs(post, dir, 'deep')).toThrow(/video is missing/);
  });

  it('makes retaining weak hashtag evidence explicit instead of silently changing engine parity', () => {
    const post: BenchPost = {
      media_type: 'image',
      caption: 'Synthetic lettering #tool #type #art #design #study',
      slides: [],
    };
    expect(benchRequest(post, true, true, 'trim').user).not.toContain('#tool');
    expect(benchRequest(post, true, true).user).toContain('#tool #type #art #design #study');
    expect(benchRequest(post, true, true, 'weak').user).toContain(
      '#tool #type #art #design #study',
    );
  });

  it('never adds unfiltered transcript properties from independent benchmark extraction', () => {
    const post: BenchPost = {
      media_type: 'video',
      caption: 'Synthetic object study',
      slides: [Object.assign({ type: 'video' }, { transcript: 'NOISY_UNFILTERED_AUDIO' })],
    };
    expect(benchRequest(post, true, true).user).not.toContain('NOISY_UNFILTERED_AUDIO');
  });

  it('refuses outputs inside a different Git checkout as well as this task repository', () => {
    const root = directory();
    const otherRepo = directory();
    expect(spawnSync('git', ['init', '--quiet', otherRepo]).status).toBe(0);
    expect(() => privateOutput(path.join(otherRepo, 'private-run'), [root])).toThrow(
      /every Git worktree/,
    );
  });
});
