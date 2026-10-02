// Formatting of the account's Settings sections (src/views/settings/format.ts).
import { describe, it, expect } from 'vitest';
import { describeUserAgent, formatBytes } from '@ui/views/settings/format';

describe('formatBytes', () => {
  it('picks the unit, with one decimal from KB up', () => {
    expect(formatBytes(0, 'en')).toBe('0 B');
    expect(formatBytes(1023, 'en')).toBe('1,023 B');
    expect(formatBytes(1536, 'en')).toBe('1.5 KB');
    expect(formatBytes(229_376, 'en')).toBe('224 KB');
    expect(formatBytes(3 * 1024 ** 3, 'en')).toBe('3 GB');
    expect(formatBytes(1536, 'it')).toBe('1,5 KB');
    expect(formatBytes(-5, 'en')).toBe('0 B');
    expect(formatBytes(Number.NaN, 'en')).toBe('0 B');
  });
});

describe('describeUserAgent', () => {
  const cases: [string, ReturnType<typeof describeUserAgent>][] = [
    [
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15',
      { browser: 'Safari', os: 'macOS' },
    ],
    [
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36',
      { browser: 'Chrome', os: 'macOS' },
    ],
    [
      'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1',
      { browser: 'Safari', os: 'iOS' },
    ],
    // A home-screen app on iOS has no `Safari/` token.
    [
      'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148',
      { browser: 'Safari', os: 'iOS' },
    ],
    [
      'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/140.0 Mobile/15E148 Safari/604.1',
      { browser: 'Chrome', os: 'iOS' },
    ],
    [
      'Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36',
      { browser: 'Chrome', os: 'Android' },
    ],
    [
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0',
      { browser: 'Edge', os: 'Windows' },
    ],
    [
      'Mozilla/5.0 (X11; Linux x86_64; rv:142.0) Gecko/20100101 Firefox/142.0',
      { browser: 'Firefox', os: 'Linux' },
    ],
    ['curl/8.7.1', null],
    ['', null],
  ];

  it('names the browser and the system', () => {
    for (const [ua, expected] of cases) expect(describeUserAgent(ua), ua).toEqual(expected);
    expect(describeUserAgent(null)).toBeNull();
  });
});
