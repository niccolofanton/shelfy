import { describe, it, expect, vi, afterEach } from 'vitest';
import {
  mdEscapeText,
  mdAutolink,
  postsToMarkdown,
  copyPostLinks,
} from '../../src/lib/exportMarkdown';

describe('mdEscapeText', () => {
  it('escapes the characters that break [text](url) link syntax', () => {
    expect(mdEscapeText('a [b] c\\d')).toBe('a \\[b\\] c\\\\d');
  });

  it('leaves plain text untouched', () => {
    expect(mdEscapeText('pasta al pomodoro #yum')).toBe('pasta al pomodoro #yum');
  });
});

describe('mdAutolink', () => {
  it('wraps the URL in angle brackets so `)` cannot close the link', () => {
    expect(mdAutolink('https://x.test/p?a=(1)')).toBe('<https://x.test/p?a=(1)>');
  });

  it('escapes `>` so the URL cannot close the autolink itself', () => {
    expect(mdAutolink('https://x.test/?q=a>b')).toBe('<https://x.test/?q=a%3Eb>');
  });
});

describe('postsToMarkdown', () => {
  it('emits one escaped bullet link per post with its tags', () => {
    const md = postsToMarkdown([
      {
        aiDescription: 'foto [al] tramonto',
        postUrl: 'https://x.test/p/(1)',
        aiTags: ['sunset', 'sea'],
      },
    ] as unknown as Shelfy.Post[]);
    expect(md).toBe('- [foto \\[al\\] tramonto](<https://x.test/p/(1)>) #sunset #sea');
  });

  it('collapses whitespace and falls back text → author → "post"', () => {
    const md = postsToMarkdown([
      { text: '  due\n righe ', postUrl: 'https://x.test/a' },
      { authorUsername: 'mario', postUrl: 'https://x.test/b' },
      { postUrl: 'https://x.test/c' },
    ] as unknown as Shelfy.Post[]);
    expect(md).toBe(
      [
        '- [due righe](<https://x.test/a>)',
        '- [mario](<https://x.test/b>)',
        '- [post](<https://x.test/c>)',
      ].join('\n'),
    );
  });

  it('renders posts without a permalink as plain bullets, not empty links', () => {
    expect(postsToMarkdown([{ aiDescription: 'solo testo' }] as unknown as Shelfy.Post[])).toBe(
      '- solo testo',
    );
  });
});

describe('copyPostLinks', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('reports empty without touching the clipboard', async () => {
    const writeText = vi.fn();
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    expect(await copyPostLinks([{ postUrl: null }] as unknown as Shelfy.Post[])).toEqual({
      status: 'empty',
      n: 0,
    });
    expect(writeText).not.toHaveBeenCalled();
  });

  it('copies one link per line and counts only posts with a URL', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    const res = await copyPostLinks([
      { postUrl: 'a' },
      {},
      { postUrl: 'b' },
    ] as unknown as Shelfy.Post[]);
    expect(writeText).toHaveBeenCalledWith('a\nb');
    expect(res).toEqual({ status: 'copied', n: 2 });
  });

  it('reports failed when the clipboard write rejects', async () => {
    vi.stubGlobal('navigator', {
      clipboard: { writeText: vi.fn().mockRejectedValue(new Error('denied')) },
    });
    expect(await copyPostLinks([{ postUrl: 'a' }] as unknown as Shelfy.Post[])).toEqual({
      status: 'failed',
      n: 1,
    });
  });
});
