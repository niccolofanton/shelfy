import net from 'net';

// ─── Shared SSRF guard ──────────────────────────────────────────────────────
//
// Single source of truth for the URL safety policy used across the app:
//   - downloader.js  (media URLs from imported, untrusted post JSON)
//   - webcapture/web-enrich/weborchestrator + the `web:add` IPC handler
//     (URLs pasted by the user / discovered via crawl)
//
// We only ever allow http(s) and reject loopback / link-local / private /
// internal hosts before issuing a request or navigating an offscreen window.
// Extracted from downloader.js so every caller enforces the same policy.

// Blocked IPv4 ranges, keyed on the two leading octets. Shared between the
// dotted-quad path and the single-integer host path below.
function isBlockedIPv4Octets(a: number, b: number): boolean {
  if (a === 127) return true; // 127.0.0.0/8 loopback
  if (a === 10) return true; // 10.0.0.0/8 private
  if (a === 0) return true; // 0.0.0.0/8
  if (a === 169 && b === 254) return true; // 169.254.0.0/16 link-local (incl. 169.254.169.254 metadata)
  if (a === 172 && b >= 16 && b <= 31) return true; // 172.16.0.0/12 private
  if (a === 192 && b === 168) return true; // 192.168.0.0/16 private
  return false;
}

// NOTE: DNS-rebinding non coperto: blocca solo letterali host/IP
function isBlockedHostname(hostname: string): boolean {
  const h = (hostname || '').toLowerCase().replace(/^\[|\]$/g, ''); // strip IPv6 brackets
  if (!h) return true;
  if (h === 'localhost' || h.endsWith('.localhost')) return true;
  if (h === '::1' || h === '::') return true;

  // Single-integer hosts (decimal `2130706433`, hex `0x7f000001`, octal
  // `017700000001`) are accepted by browsers/Node as IPv4 addresses, so they
  // must go through the same range checks as dotted quads — otherwise
  // 127.0.0.1 sneaks past as a number.
  if (net.isIP(h) === 0 && /^(?:0x[0-9a-f]+|0[0-7]*|[1-9]\d*)$/.test(h)) {
    const n = h.startsWith('0x')
      ? parseInt(h, 16)
      : /^0[0-7]+$/.test(h)
        ? parseInt(h, 8)
        : parseInt(h, 10);
    if (Number.isFinite(n) && n >= 0 && n <= 0xffffffff) {
      return isBlockedIPv4Octets((n >>> 24) & 0xff, (n >>> 16) & 0xff);
    }
    return true; // numeric host out of IPv4 range — refuse outright
  }

  // IPv4-mapped / link-local / metadata / loopback / private ranges. The optional
  // `::` (with or without the `ffff:` marker) also covers the deprecated
  // IPv4-compatible dotted form `::a.b.c.d` (e.g. ::127.0.0.1), which would
  // otherwise slip past to the loose prefix checks below.
  const v4 = h.match(/^(?:::(?:ffff:)?)?(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (v4) {
    return isBlockedIPv4Octets(Number(v4[1]), Number(v4[2]));
  }

  // IPv4-mapped IPv6 (::ffff:a.b.c.d) may also arrive in hex-compressed form
  // (::ffff:7f00:1 === 127.0.0.1), which net.isIP() reports as a valid IPv6.
  // Both encode the same embedded IPv4, so normalize either tail to its two
  // leading octets and run the shared range check — otherwise the hex form
  // sneaks past the dotted regex above and reaches the loose prefix checks
  // below (which only see leading zeros and return false).
  if (net.isIP(h) === 6 && /^(0*:){0,5}(0*:)?ffff:/i.test(h)) {
    const tail = h.slice(h.lastIndexOf('ffff:') + 5);
    if (tail.includes('.')) {
      const m = tail.match(/^(\d{1,3})\.(\d{1,3})\./);
      // Mapped address with a malformed dotted tail: refuse rather than allow.
      return m ? isBlockedIPv4Octets(Number(m[1]), Number(m[2])) : true;
    }
    const hextets = tail.split(':');
    const hi = hextets.length === 2 ? parseInt(hextets[0], 16) : NaN;
    if (Number.isFinite(hi)) {
      return isBlockedIPv4Octets((hi >>> 8) & 0xff, hi & 0xff);
    }
    return true; // unexpected mapped form — refuse outright
  }

  // IPv6 unique-local (fc00::/7) and link-local (fe80::/10).
  if (/^f[cd][0-9a-f]{2}:/.test(h)) return true;
  if (/^fe[89ab][0-9a-f]:/.test(h)) return true;
  return false;
}

// Validates an external URL, returning the parsed URL or throwing. Only
// http/https schemes are allowed, and internal/loopback hosts are rejected.
function assertSafeUrl(rawUrl: string): URL {
  let u: URL;
  try {
    u = new URL(rawUrl);
  } catch {
    throw new Error('Invalid URL');
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') {
    throw new Error(`Refusing non-http(s) URL scheme: ${u.protocol}`);
  }
  if (isBlockedHostname(u.hostname)) {
    throw new Error(`Refusing internal/loopback host: ${u.hostname}`);
  }
  return u;
}

export { isBlockedHostname, assertSafeUrl };
// Back-compat alias: downloader.js historically named this assertSafeMediaUrl.
export const assertSafeMediaUrl = assertSafeUrl;
