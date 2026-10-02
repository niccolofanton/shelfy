// Formatting for the account's Settings sections: sizes, dates, and the
// browser and system a User-Agent names.
import { localeTag } from '../../i18n';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

// `bytes` in the largest unit that keeps it at or above 1 (1 KB = 1024 B),
// with one decimal from KB up.
export function formatBytes(bytes: number, lang: string): string {
  let value = Math.max(0, Number.isFinite(bytes) ? bytes : 0);
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : 1;
  const number = new Intl.NumberFormat(localeTag(lang), {
    minimumFractionDigits: 0,
    maximumFractionDigits: digits,
  }).format(value);
  return `${number} ${UNITS[unit]}`;
}

// A moment (unix ms) as a short date and time.
export function formatDateTime(ms: number, lang: string): string {
  return new Date(ms).toLocaleString(localeTag(lang), {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
}

// A moment (unix ms) as a date.
export function formatDate(ms: number, lang: string): string {
  return new Date(ms).toLocaleDateString(localeTag(lang), { dateStyle: 'medium' });
}

export interface DeviceName {
  browser: string | null;
  os: string | null;
}

// The browser and the system of a User-Agent, for people: "Chrome" and
// "macOS". Null when it names neither (a script, an empty value).
export function describeUserAgent(userAgent: string | null | undefined): DeviceName | null {
  const ua = userAgent ?? '';
  if (!ua) return null;
  let browser = '';
  if (/\bEdg(?:e|A|iOS)?\//.test(ua)) browser = 'Edge';
  else if (/\bOPR\/|\bOpera\b/.test(ua)) browser = 'Opera';
  else if (/\bSamsungBrowser\//.test(ua)) browser = 'Samsung Internet';
  else if (/\bFirefox\/|\bFxiOS\//.test(ua)) browser = 'Firefox';
  else if (/\bCriOS\/|\bChrome\/|\bChromium\//.test(ua)) browser = 'Chrome';
  else if (/\bSafari\//.test(ua) && /\bVersion\//.test(ua)) browser = 'Safari';
  else if (/AppleWebKit\//.test(ua) && /\b(iPhone|iPad|iPod)\b/.test(ua)) browser = 'Safari';

  let os = '';
  if (/\b(iPhone|iPod)\b/.test(ua)) os = 'iOS';
  else if (/\biPad\b/.test(ua)) os = 'iPadOS';
  else if (/\bAndroid\b/.test(ua)) os = 'Android';
  else if (/\bCrOS\b/.test(ua)) os = 'ChromeOS';
  else if (/\bMac OS X\b|\bMacintosh\b/.test(ua)) os = 'macOS';
  else if (/\bWindows\b/.test(ua)) os = 'Windows';
  else if (/\bLinux\b/.test(ua)) os = 'Linux';

  if (!browser && !os) return null;
  return { browser: browser || null, os: os || null };
}
