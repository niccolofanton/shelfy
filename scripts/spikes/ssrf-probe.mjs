#!/usr/bin/env node
/**
 * SPIKE-4 SSRF probe suite for the capture container (plan §2.18, §6.1, §7.1).
 *
 * Runs INSIDE a container configured like `shelfy-capture`: only on the internal
 * capture network, with the egress proxy (Smokescreen) as its one way out. Every
 * probe below must be refused; the positive controls must pass, to show that the
 * refusals come from the policy and not from a broken setup.
 *
 *   A  controls      HTTPS (CONNECT), HTTP, WSS and Node fetch to public hosts work
 *   B  direct IPs    every denied range, the Docker gateways, the proxy and a canary
 *   C  IPv6          literals: loopback, IPv4-mapped (dotted and hex), link-local,
 *                    ULA, NAT64, 6to4, IPv4-compatible
 *   D  encodings     decimal, octal, hex and short IPv4 forms, in Chromium (which
 *                    canonicalizes them) and raw (which the proxy must parse itself)
 *   E  DNS           public names that resolve to internal addresses, internal names
 *   F  redirects     a public host answering 302 to an internal address
 *   G  ports         public hosts on ports other than 80 and 443
 *   H  WebSockets    ws:// and wss:// to internal hosts, from a Chromium page
 *   I  self          the VPS's own public addresses and its tailnet address
 *   J  direct        TCP, UDP and DNS straight from the container (no proxy), the
 *                    host's addresses on the Docker bridges, WebRTC/STUN, and
 *                    Chromium's loopback bypass (with a negative control that
 *                    shows why it matters)
 *   K  rebinding     names that alternate between 127.0.0.1 and a public address
 *   L  chaining      asking Smokescreen to chain to another proxy
 *
 * Probes go out three ways: raw requests to the proxy (Node `net`, so the proxy
 * sees exactly the bytes we choose), Chromium through Playwright (sandbox on,
 * launched with the capture service's proxy settings), and direct sockets.
 * A refusal is a 407 from the proxy ("Egress proxying is denied…"), a DNS
 * failure at the proxy, a browser network error, or no route. `analyze` then
 * checks the proxy's own decision log (no allowed connection to a non-public
 * address or to a port other than 80 and 443) and the canary's log (nothing
 * reached it through the proxy).
 *
 * Usage (inside the probe container; see scripts/spikes/capture-vps.sh):
 *   node ssrf-probe.mjs probe --proxy http://shelfy-spike4-egress:4750 --out results.json \
 *     [--canary-host NAME --canary-ips IP,IP] [--proxy-ips IP,IP] [--gateways IP,...] \
 *     [--self-ips IP,...] [--only A,B,...]
 *   node ssrf-probe.mjs canary [--ports 80,443,8080] [--udp 53]   # logs every hit as JSON
 *   node ssrf-probe.mjs sandbox [--proxy URL]                    # launch Chromium, print its sandbox layers
 *   node ssrf-probe.mjs analyze --results results.json --proxy-log egress.log \
 *     --canary-log canary.log [--proxy-ips IP,IP] [--markdown]
 *
 * Chromium probes need playwright-core next to this file (node_modules/) and
 * PLAYWRIGHT_BROWSERS_PATH; without it they are skipped and reported as such.
 * Personal data: none. Targets are public test services and reserved addresses;
 * the VPS's addresses come from --self-ips and appear only as <self-N>.
 */

// The page.evaluate() callbacks below run in Chromium, not in Node.
/* global document, RTCPeerConnection */

import fs from 'fs';
import net from 'net';
import path from 'path';
import dgram from 'dgram';
import http from 'http';
import dnsPromises from 'dns/promises';
import { createRequire } from 'module';
import { fileURLToPath } from 'url';

const HERE = path.dirname(fileURLToPath(import.meta.url));

// ─── CLI ─────────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const [mode = 'probe', ...rest] = argv;
  const opts = { mode };
  for (let i = 0; i < rest.length; i++) {
    const k = rest[i];
    if (!k.startsWith('--')) throw new Error(`unexpected argument ${k}`);
    const key = k.slice(2);
    const next = rest[i + 1];
    if (next === undefined || next.startsWith('--')) opts[key] = true;
    else {
      opts[key] = next;
      i++;
    }
  }
  return opts;
}

const list = (v) =>
  typeof v === 'string'
    ? v
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean)
    : [];

// ─── Address classification (for the analysis) ─────────────────────────────

function ipv4ToInt(ip) {
  const p = ip.split('.').map(Number);
  return ((p[0] << 24) >>> 0) + (p[1] << 16) + (p[2] << 8) + p[3];
}

const DENIED_V4 = [
  '0.0.0.0/8',
  '10.0.0.0/8',
  '100.64.0.0/10',
  '127.0.0.0/8',
  '169.254.0.0/16',
  '172.16.0.0/12',
  '192.0.0.0/24',
  '192.0.2.0/24',
  '192.88.99.0/24',
  '192.168.0.0/16',
  '198.18.0.0/15',
  '198.51.100.0/24',
  '203.0.113.0/24',
  '224.0.0.0/4',
  '240.0.0.0/4',
].map((c) => {
  const [base, bits] = c.split('/');
  const mask = Number(bits) === 0 ? 0 : (0xffffffff << (32 - Number(bits))) >>> 0;
  return { cidr: c, base: ipv4ToInt(base) & mask, mask };
});

function isPublicAddress(ip, extraDenied = []) {
  if (extraDenied.includes(ip)) return false;
  if (net.isIPv4(ip)) {
    const n = ipv4ToInt(ip);
    return !DENIED_V4.some((r) => (n & r.mask) >>> 0 === r.base);
  }
  if (net.isIPv6(ip)) {
    const h = ip.toLowerCase();
    if (h === '::1' || h === '::' || h.startsWith('::ffff:')) return false;
    return !/^(f[cd]|fe[89ab]|fec|fed|fee|fef|ff|64:ff9b|2002:|2001:0?db8|2001:0{0,4}:|100:)/.test(
      h,
    );
  }
  return false;
}

// ─── Raw proxy requests ──────────────────────────────────────────────────────

let traceSeq = 0;

// Sends one request to the proxy and reads its answer (status line, headers and
// the first bytes of the body). For CONNECT, a 200 means the tunnel is open: we
// close it right away and never speak to the destination.
function rawProxy(proxyUrl, requestLine, { headers = {}, timeoutMs = 15_000 } = {}) {
  const p = new URL(proxyUrl);
  const trace = `probe-${process.pid}-${++traceSeq}`;
  return new Promise((resolve) => {
    const sock = net.connect({ host: p.hostname, port: Number(p.port || 80) });
    let buf = Buffer.alloc(0);
    let done = false;
    const finish = (r) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      sock.destroy();
      resolve({ trace, ...r });
    };
    const timer = setTimeout(() => finish({ kind: 'timeout' }), timeoutMs);
    sock.on('connect', () => {
      // The Host header repeats the target verbatim (no URL normalization), so the
      // proxy parses exactly the encoding under test.
      const [method, target] = requestLine.split(' ');
      const host =
        method === 'CONNECT' ? target : target.replace(/^[a-z]+:\/\//i, '').split('/')[0];
      const lines = [
        `${requestLine} HTTP/1.1`,
        `Host: ${host}`,
        `X-Smokescreen-Trace-ID: ${trace}`,
      ];
      for (const [k, v] of Object.entries(headers)) lines.push(`${k}: ${v}`);
      lines.push('Connection: close', '', '');
      sock.write(lines.join('\r\n'));
    });
    sock.on('data', (d) => {
      buf = Buffer.concat([buf, d]);
      const text = buf.toString('latin1');
      const end = text.indexOf('\r\n\r\n');
      if (end === -1) return;
      const head = text.slice(0, end).split('\r\n');
      const status = Number((/^HTTP\/1\.[01] (\d{3})/.exec(head[0]) || [])[1] || 0);
      const hdrs = {};
      for (const h of head.slice(1)) {
        const i = h.indexOf(':');
        if (i > 0) hdrs[h.slice(0, i).trim().toLowerCase()] = h.slice(i + 1).trim();
      }
      const body = text.slice(end + 4, end + 4 + 400);
      if (requestLine.startsWith('CONNECT') && status === 200)
        return finish({ kind: 'status', status, headers: hdrs, body: '' });
      if (body.length >= 200 || buf.length > 64_000)
        return finish({ kind: 'status', status, headers: hdrs, body });
      // Wait briefly for the rest of a short error body.
      setTimeout(
        () =>
          finish({
            kind: 'status',
            status,
            headers: hdrs,
            body: buf.toString('latin1').slice(end + 4, end + 404),
          }),
        300,
      );
    });
    sock.on('error', (e) => finish({ kind: 'error', code: e.code || e.message }));
    sock.on('close', () => {
      if (!done && buf.length === 0) finish({ kind: 'error', code: 'closed' });
    });
  });
}

// The rule behind a Smokescreen refusal, shortened for the tables.
function denyReason(text) {
  const t = String(text || '');
  const rule = /denied by rule '([^']+)'/.exec(t);
  if (rule) return rule[1].replace(/^Deny: /, 'address: ');
  const m = /Egress proxying is denied to host '[^']*': (.*?)\.?$/.exec(t);
  return (m ? m[1] : t).slice(0, 160);
}

// Classifies a proxy answer: refused by policy, refused at DNS, or let through.
function classifyProxy(r) {
  if (r.kind === 'timeout') return { outcome: 'timeout', detail: 'no answer from the proxy' };
  if (r.kind === 'error') return { outcome: 'error', detail: r.code };
  const reason =
    r.headers['x-smokescreen-error'] ||
    (/Egress proxying is denied[^\n]*/.exec(r.body) || [])[0] ||
    '';
  if (r.status === 407) return { outcome: 'refused-policy', detail: denyReason(reason) };
  // A name with no usable address (e.g. AAAA only, with `network: ip4`) fails
  // before any dial: Smokescreen reports it as a connect error (net.AddrError).
  if (r.status === 502 && /resolve|AddrError|no suitable address/i.test(reason + r.body))
    return { outcome: 'refused-dns', detail: reason.slice(0, 200) || 'DNS failure' };
  if (r.status === 400)
    return { outcome: 'refused-bad-request', detail: (reason || r.body).slice(0, 120) };
  if (r.status === 502 || r.status === 504)
    return { outcome: 'allowed-connect-failed', detail: (reason || r.body).slice(0, 200) };
  if (r.status >= 200 && r.status < 400) return { outcome: 'allowed', detail: `HTTP ${r.status}` };
  return { outcome: 'other', detail: `HTTP ${r.status} ${(reason || r.body).slice(0, 120)}` };
}

const REFUSED = new Set([
  'refused-policy',
  'refused-dns',
  'refused-bad-request',
  'browser-error',
  'unreachable',
  'ws-failed',
  'no-srflx',
  'not-resolved',
  'not-reached',
]);

// ─── Direct sockets ──────────────────────────────────────────────────────────

function tcpConnect(host, port, timeoutMs = 5000) {
  return new Promise((resolve) => {
    const s = net.connect({ host, port });
    const t = setTimeout(() => {
      s.destroy();
      resolve({ outcome: 'unreachable', detail: 'timeout' });
    }, timeoutMs);
    s.on('connect', () => {
      clearTimeout(t);
      s.destroy();
      resolve({ outcome: 'connected', detail: 'TCP handshake completed' });
    });
    s.on('error', (e) => {
      clearTimeout(t);
      resolve({ outcome: 'unreachable', detail: e.code || e.message });
    });
  });
}

function udpDnsQuery(server, timeoutMs = 4000) {
  // A minimal DNS query for example.com A.
  const q = Buffer.from('abcd01000001000000000000076578616d706c6503636f6d0000010001', 'hex');
  return new Promise((resolve) => {
    const s = dgram.createSocket('udp4');
    const t = setTimeout(() => {
      s.close();
      resolve({ outcome: 'unreachable', detail: 'no reply' });
    }, timeoutMs);
    s.on('message', () => {
      clearTimeout(t);
      s.close();
      resolve({ outcome: 'connected', detail: 'DNS reply received' });
    });
    s.on('error', (e) => {
      clearTimeout(t);
      s.close();
      resolve({ outcome: 'unreachable', detail: e.code || e.message });
    });
    s.send(q, 53, server, (e) => {
      if (e) {
        clearTimeout(t);
        s.close();
        resolve({ outcome: 'unreachable', detail: e.code || e.message });
      }
    });
  });
}

// ─── Chromium ────────────────────────────────────────────────────────────────

function loadPlaywright() {
  try {
    return createRequire(path.join(HERE, 'package.json'))('playwright-core');
  } catch {
    return null;
  }
}

const EGRESS_ARGS = ['--disable-quic', '--force-webrtc-ip-handling-policy=disable_non_proxied_udp'];

// Launches Chromium the way the capture service does: sandbox on, the proxy
// through Playwright's `proxy` option (which adds --proxy-bypass-list=<-loopback>).
// `rawProxyFlag` reproduces the plan's literal `--proxy-server` flag instead.
async function launchChromium(pw, proxy, { rawProxyFlag = false } = {}) {
  const args = [
    '--use-gl=angle',
    '--use-angle=swiftshader',
    '--enable-unsafe-swiftshader',
    '--disable-dev-shm-usage',
    ...EGRESS_ARGS,
  ];
  if (rawProxyFlag) args.push(`--proxy-server=${proxy}`);
  return pw.chromium.launch({
    headless: true,
    chromiumSandbox: process.env.SHELFY_DISABLE_SANDBOX !== '1',
    args,
    ...(rawProxyFlag ? {} : { proxy: { server: proxy } }),
  });
}

// An http:// origin served by Playwright's router (no network), so pages can
// open ws:// sockets without mixed-content blocking.
const PROBE_ORIGIN = 'http://probe.shelfy.test';

async function newProbePage(browser) {
  const context = await browser.newContext({ serviceWorkers: 'block' });
  await context.route(`${PROBE_ORIGIN}/**`, (route) =>
    route.fulfill({
      status: 200,
      contentType: 'text/html',
      body: '<!doctype html><title>probe</title>',
    }),
  );
  const page = await context.newPage();
  await page.goto(`${PROBE_ORIGIN}/`);
  return { context, page };
}

async function chromiumNav(browser, url, timeoutMs = 20_000) {
  const context = await browser.newContext({ serviceWorkers: 'block' });
  const page = await context.newPage();
  try {
    const resp = await page.goto(url, { timeout: timeoutMs, waitUntil: 'domcontentloaded' });
    if (!resp) return { outcome: 'browser-error', detail: 'no response' };
    const headers = await resp.allHeaders().catch(() => ({}));
    const status = resp.status();
    const final = page.url();
    if (headers['x-smokescreen-error'] || status === 407)
      return {
        outcome: 'refused-policy',
        detail: `HTTP ${status} ${denyReason(headers['x-smokescreen-error'])}`,
      };
    if (status === 502 || status === 504) {
      const body = (await resp.text().catch(() => '')).slice(0, 200);
      if (/resolve/i.test(body)) return { outcome: 'refused-dns', detail: body };
      return { outcome: 'allowed-connect-failed', detail: `HTTP ${status} ${body}` };
    }
    return { outcome: 'allowed', detail: `HTTP ${status} at ${final.slice(0, 80)}` };
  } catch (e) {
    const msg = String(e?.message || e).split('\n')[0];
    const code = (/net::[A-Z_]+/.exec(msg) || [msg.slice(0, 120)])[0];
    return { outcome: 'browser-error', detail: code };
  } finally {
    await context.close().catch(() => {});
  }
}

async function chromiumWebSockets(browser, urls, timeoutMs = 10_000) {
  const { context, page } = await newProbePage(browser);
  try {
    return await page.evaluate(
      ({ urls, timeoutMs }) =>
        Promise.all(
          urls.map(
            (u) =>
              new Promise((res) => {
                let ws;
                try {
                  ws = new WebSocket(u);
                } catch (e) {
                  res({ url: u, state: 'throw', detail: String(e).slice(0, 100) });
                  return;
                }
                const t = setTimeout(() => {
                  try {
                    ws.close();
                  } catch {}
                  res({ url: u, state: 'timeout' });
                }, timeoutMs);
                ws.onopen = () => {
                  clearTimeout(t);
                  ws.send('shelfy-probe');
                  setTimeout(() => {
                    ws.close();
                  }, 500);
                  res({ url: u, state: 'open' });
                };
                ws.onclose = (ev) => {
                  clearTimeout(t);
                  res({ url: u, state: 'closed', code: ev.code });
                };
              }),
          ),
        ),
      { urls, timeoutMs },
    );
  } finally {
    await context.close().catch(() => {});
  }
}

async function chromiumWebRtc(browser) {
  const { context, page } = await newProbePage(browser);
  try {
    return await page.evaluate(async () => {
      const pc = new RTCPeerConnection({ iceServers: [{ urls: 'stun:stun.l.google.com:19302' }] });
      pc.createDataChannel('probe');
      const cands = [];
      pc.onicecandidate = (e) => {
        if (e.candidate && e.candidate.candidate) cands.push(e.candidate.candidate);
      };
      await pc.setLocalDescription(await pc.createOffer());
      await new Promise((r) => setTimeout(r, 6000));
      pc.close();
      return cands;
    });
  } finally {
    await context.close().catch(() => {});
  }
}

// ─── Sandbox report (same logic as capture-harness/src/chromium.ts) ─────────

function chromiumProcesses() {
  const out = [];
  for (const pid of fs.readdirSync('/proc').filter((d) => /^\d+$/.test(d))) {
    let cmd;
    let status;
    try {
      // Chromium rewrites the titles of forked children: argv may be one string.
      cmd = fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').replace(/\0/g, ' ').trim();
      status = fs.readFileSync(`/proc/${pid}/status`, 'utf8');
    } catch {
      continue;
    }
    // chrome-headless-shell on linux64, headless_shell on arm64.
    if (
      !/^(chrome-headless-shell|headless_shell|chrome|chromium)$/.test(
        path.basename(cmd.split(' ')[0] || ''),
      )
    )
      continue;
    const type = /--type=(\S+)/.exec(cmd)?.[1] || 'browser';
    const sub = /--utility-sub-type=(\S+)/.exec(cmd)?.[1];
    const ns = {};
    for (const k of ['user', 'pid', 'net']) {
      try {
        ns[k] = fs.readlinkSync(`/proc/${pid}/ns/${k}`);
      } catch {
        ns[k] = 'unreadable';
      }
    }
    out.push({
      type: sub ? `${type}:${sub.split('.').pop()}` : type,
      filters: Number(/^Seccomp_filters:\s+(\d+)/m.exec(status)?.[1] || 0),
      ns,
    });
  }
  return out;
}

// Per Chromium process type: how many run in their own user, PID and network
// namespaces and under an extra seccomp filter, compared with the browser
// process (same logic as capture-harness/src/chromium.ts).
function sandboxReport() {
  if (process.platform !== 'linux') return [];
  const procs = chromiumProcesses();
  const base = procs.find((p) => p.type === 'browser');
  if (!base) return [];
  const byType = new Map();
  for (const p of procs) {
    const row = byType.get(p.type) || {
      type: p.type,
      count: 0,
      ownUserNs: 0,
      ownPidNs: 0,
      ownNetNs: 0,
      extraSeccomp: 0,
    };
    row.count++;
    if (p.ns.user !== base.ns.user) row.ownUserNs++;
    if (p.ns.pid !== base.ns.pid) row.ownPidNs++;
    if (p.ns.net !== base.ns.net) row.ownNetNs++;
    if (p.filters > base.filters) row.extraSeccomp++;
    byType.set(p.type, row);
  }
  return [...byType.values()].sort((a, b) => a.type.localeCompare(b.type));
}

// ─── Probe catalogue ─────────────────────────────────────────────────────────

const REDIRECTORS = [
  'https://httpbin.org/redirect-to?url=',
  'https://httpbingo.org/redirect-to?url=',
];
const WS_ECHO = ['wss://ws.postman-echo.com/raw', 'wss://echo.websocket.org/'];

// The VPS's own addresses never appear in the output: --self-ips entry i is
// shown as <self-i> (capture-vps.sh passes public IPv4, public IPv6, tailnet IPv4).
function mask(s, selfIps) {
  let out = String(s);
  // Longest first, so an address that prefixes another is not half-replaced.
  [...selfIps.entries()]
    .sort((a, b) => b[1].length - a[1].length)
    .forEach(([i, ip]) => {
      out = out.split(ip).join(`<self-${i}>`);
    });
  return out;
}

async function runProbes(opts) {
  const proxy = opts.proxy;
  if (!proxy) throw new Error('--proxy is required');
  const only = new Set(list(opts.only));
  const want = (cat) => !only.size || only.has(cat);
  const canaryHost = opts['canary-host'] || null;
  const canaryIps = list(opts['canary-ips']);
  const proxyIps = list(opts['proxy-ips']);
  const gateways = list(opts.gateways);
  const selfIps = list(opts['self-ips']);
  const results = [];
  const record = (cat, id, via, target, expect, r) => {
    const refused = REFUSED.has(r.outcome);
    const verdict =
      expect === 'info'
        ? 'info'
        : expect === 'refuse'
          ? refused
            ? 'pass'
            : 'FAIL'
          : r.outcome === 'allowed' || r.outcome === 'open' || r.outcome === 'connected'
            ? 'pass'
            : 'FAIL';
    const row = {
      cat,
      id,
      via,
      target: mask(target, selfIps),
      expect,
      outcome: r.outcome,
      detail: mask(r.detail ?? '', selfIps),
      trace: r.trace,
      verdict,
    };
    results.push(row);
    process.stdout.write(
      `${verdict.padEnd(4)} ${cat}/${id.padEnd(22)} ${via.padEnd(12)} ${row.target.slice(0, 60).padEnd(60)} ${r.outcome} ${row.detail.slice(0, 90)}\n`,
    );
  };
  const raw = async (cat, id, line, expect = 'refuse', headers) => {
    const r = await rawProxy(proxy, line, { headers });
    record(cat, id, line.startsWith('CONNECT') ? 'raw-connect' : 'raw-http', line, expect, {
      ...classifyProxy(r),
      trace: r.trace,
    });
  };

  const pw = loadPlaywright();
  let browser = null;
  let sandbox = [];
  if (pw) {
    browser = await launchChromium(pw, proxy);
    // Snapshot the sandbox while a renderer is alive.
    const { context } = await newProbePage(browser);
    await new Promise((r) => setTimeout(r, 500));
    sandbox = sandboxReport();
    await context.close();
  } else {
    process.stdout.write(
      'playwright-core not found next to this script: Chromium probes skipped\n',
    );
  }
  const nav = async (cat, id, url, expect = 'refuse') => {
    if (!browser)
      return record(cat, id, 'chromium-nav', url, 'info', {
        outcome: 'skipped',
        detail: 'no Chromium',
      });
    record(cat, id, 'chromium-nav', url, expect, await chromiumNav(browser, url));
  };

  // A — positive controls
  if (want('A')) {
    await raw('A', 'connect-443', 'CONNECT example.com:443', 'allow');
    await raw('A', 'http-80', 'GET http://example.com/', 'allow');
    await nav('A', 'https', 'https://example.com/', 'allow');
    await nav('A', 'http', 'http://example.com/', 'allow');
    try {
      const res = await fetch('https://example.com/', { signal: AbortSignal.timeout(15_000) });
      record('A', 'node-fetch', 'node-fetch', 'https://example.com/', 'allow', {
        outcome: res.ok ? 'allowed' : 'other',
        detail: `HTTP ${res.status} (NODE_USE_ENV_PROXY=${process.env.NODE_USE_ENV_PROXY || ''})`,
      });
    } catch (e) {
      record('A', 'node-fetch', 'node-fetch', 'https://example.com/', 'allow', {
        outcome: 'error',
        detail: e?.cause?.code || e.message,
      });
    }
    if (browser) {
      let ok = null;
      for (const u of WS_ECHO) {
        const [r] = await chromiumWebSockets(browser, [u]);
        if (r.state === 'open') {
          ok = { outcome: 'open', detail: u };
          break;
        }
        ok = { outcome: 'ws-failed', detail: `${u} ${r.state} ${r.code ?? ''}` };
      }
      record('A', 'wss-echo', 'chromium-ws', WS_ECHO.join(' | '), 'allow', ok);
    }
  }

  // B — direct IPs in denied ranges (and the Docker networks, the proxy, the canary)
  if (want('B')) {
    const ips = [
      ['loopback', '127.0.0.1'],
      ['loopback-other', '127.0.0.53'],
      ['unspecified', '0.0.0.0'],
      ['this-network', '0.1.2.3'],
      ['rfc1918-10', '10.0.0.1'],
      ['rfc1918-172', '172.16.0.1'],
      ['rfc1918-192', '192.168.0.1'],
      ['metadata', '169.254.169.254'],
      ['link-local', '169.254.1.1'],
      ['cgnat', '100.64.0.1'],
      ['tailscale-dns', '100.100.100.100'],
      ['benchmark', '198.18.0.1'],
      ['ietf-192.0.0', '192.0.0.1'],
      ['test-net-1', '192.0.2.1'],
      ['multicast', '224.0.0.1'],
      ['reserved-240', '240.0.0.1'],
      ['broadcast', '255.255.255.255'],
      ...gateways.map((g, i) => [`docker-gw-${i}`, g]),
      ...proxyIps.map((g, i) => [`proxy-self-${i}`, g]),
      ...canaryIps.map((g, i) => [`canary-${i}`, g]),
    ];
    for (const [id, ip] of ips) {
      await raw('B', `${id}-http`, `GET http://${ip}/`);
      await raw('B', `${id}-connect`, `CONNECT ${ip}:443`);
    }
    for (const [id, ip] of ips.filter(([id]) =>
      /^(loopback|metadata|rfc1918-10|cgnat|docker-gw-0|canary-0|proxy-self-0)$/.test(id),
    ))
      await nav('B', `${id}-nav`, `http://${ip}/`);
    await raw('B', 'proxy-loopback-port', 'CONNECT 127.0.0.1:4750');
  }

  // C — IPv6 literals
  if (want('C')) {
    const v6 = [
      ['loopback', '::1'],
      ['unspecified', '::'],
      ['mapped-dotted', '::ffff:127.0.0.1'],
      ['mapped-hex', '::ffff:7f00:1'],
      ['mapped-metadata', '::ffff:a9fe:a9fe'],
      ['mapped-rfc1918', '::ffff:10.0.0.1'],
      ['v4-compatible', '::127.0.0.1'],
      ['link-local', 'fe80::1'],
      ['ula-fc', 'fc00::1'],
      ['ula-fd', 'fd00::1'],
      ['nat64', '64:ff9b::7f00:1'],
      ['nat64-local', '64:ff9b:1::a9fe:a9fe'],
      ['6to4', '2002:7f00:1::1'],
      ['teredo', '2001::1'],
      ['site-local', 'fec0::1'],
    ];
    for (const [id, ip] of v6) {
      await raw('C', `${id}-http`, `GET http://[${ip}]/`);
      await raw('C', `${id}-connect`, `CONNECT [${ip}]:443`);
    }
    await nav('C', 'loopback-nav', 'http://[::1]/');
    await nav('C', 'mapped-nav', 'http://[::ffff:127.0.0.1]/');
    await nav('C', 'mapped-meta-nav', 'http://[::ffff:a9fe:a9fe]/');
  }

  // D — decimal, octal, hex and short IPv4 encodings
  if (want('D')) {
    const enc = [
      ['decimal-loopback', '2130706433'],
      ['hex-loopback', '0x7f000001'],
      ['octal-loopback', '017700000001'],
      ['dotted-octal', '0177.0.0.1'],
      ['short-127.1', '127.1'],
      ['hex-short', '0x7f.1'],
      ['decimal-metadata', '2852039166'],
      ['octal-metadata', '0251.0376.0251.0376'],
      ['hex-rfc1918', '0x0a000001'],
      ['zero-padded', '127.000.000.001'],
    ];
    for (const [id, host] of enc) {
      await raw('D', `${id}-http`, `GET http://${host}/`);
      await raw('D', `${id}-connect`, `CONNECT ${host}:443`);
      await nav('D', `${id}-nav`, `http://${host}/`);
    }
  }

  // E — DNS names resolving to internal addresses, and internal names
  if (want('E')) {
    const names = [
      ['nip-loopback', '127.0.0.1.nip.io'],
      ['localtest-me', 'localtest.me'],
      ['nip-metadata', '169.254.169.254.nip.io'],
      ['nip-rfc1918', '10.0.0.1.nip.io'],
      ['nip-192', '192.168.1.1.nip.io'],
      ['nip-cgnat', '100.64.0.1.nip.io'],
      ['nip-docker0', '172.17.0.1.nip.io'],
      ['nip-zero', '0.0.0.0.nip.io'],
      ['sslip-v6-loopback', '--1.sslip.io'],
      ['localhost', 'localhost'],
      ['localhost-sub', 'foo.localhost'],
      ['internal-tld', 'metadata.google.internal'],
      ['docker-host', 'host.docker.internal'],
      ['ip6-localhost', 'ip6-localhost'],
      ...(canaryHost ? [['container-name', canaryHost]] : []),
      ...canaryIps.slice(0, 1).map((ip) => ['nip-canary', `${ip}.nip.io`]),
    ];
    for (const [id, name] of names) {
      await raw('E', `${id}-http`, `GET http://${name}/`);
      await raw('E', `${id}-connect`, `CONNECT ${name}:443`);
    }
    for (const [id, name] of names.filter(([id]) =>
      /^(nip-loopback|localtest-me|nip-metadata|localhost|container-name)$/.test(id),
    ))
      await nav('E', `${id}-nav`, `http://${name}/`);
  }

  // F — redirects from a public host to an internal address
  if (want('F')) {
    const targets = [
      ['metadata', 'http://169.254.169.254/latest/meta-data/'],
      ['loopback-https', 'https://127.0.0.1/'],
      ['rfc1918', 'http://10.0.0.1/'],
      ...(canaryIps[0] ? [['canary', `http://${canaryIps[0]}/`]] : []),
    ];
    let redirector = REDIRECTORS[0];
    for (const r of REDIRECTORS) {
      const probe = await rawProxy(proxy, `CONNECT ${new URL(r).host}:443`);
      if (probe.kind === 'status' && probe.status === 200) {
        redirector = r;
        break;
      }
    }
    for (const [id, t] of targets) {
      const url = redirector + encodeURIComponent(t);
      await nav('F', `${id}-nav`, url);
      try {
        const res = await fetch(url, { signal: AbortSignal.timeout(20_000) });
        const err = res.headers.get('x-smokescreen-error');
        const outcome =
          res.status === 407 || err
            ? 'refused-policy'
            : res.status === 502 || res.status === 504
              ? 'allowed-connect-failed'
              : 'allowed';
        record('F', `${id}-fetch`, 'node-fetch', url, 'refuse', {
          outcome,
          detail: `HTTP ${res.status} ${denyReason(err)}`,
        });
      } catch (e) {
        record('F', `${id}-fetch`, 'node-fetch', url, 'refuse', {
          outcome: 'browser-error',
          detail: `fetch failed: ${e?.cause?.code || e?.cause?.message || e.message}`.slice(0, 160),
        });
      }
    }
  }

  // G — ports other than 80 and 443
  if (want('G')) {
    for (const [id, line] of [
      ['https-8443', 'CONNECT example.com:8443'],
      ['ssh-22', 'CONNECT github.com:22'],
      ['smtp-25', 'CONNECT smtp.gmail.com:25'],
      ['redis-6379', 'CONNECT portquiz.net:6379'],
      ['proxy-4750', 'CONNECT portquiz.net:4750'],
      ['http-8080', 'GET http://portquiz.net:8080/'],
      ['http-81', 'GET http://portquiz.net:81/'],
      ['dns-53', 'CONNECT dns.google:53'],
    ])
      await raw('G', id, line);
    await nav('G', 'http-8080-nav', 'http://portquiz.net:8080/');
    await nav('G', 'https-8443-nav', 'https://portquiz.net:8443/');
  }

  // H — WebSockets to internal hosts
  if (want('H') && browser) {
    const urls = [
      'ws://127.0.0.1:4750/',
      'ws://127.0.0.1:18081/',
      'ws://localhost:8080/',
      'ws://169.254.169.254/',
      'wss://10.0.0.1/',
      'ws://[::1]:80/',
      'ws://127.0.0.1.nip.io/',
      ...canaryIps.map((ip) => `ws://${ip}:80/`),
      ...canaryIps.map((ip) => `wss://${ip}:443/`),
      ...(canaryHost ? [`ws://${canaryHost}:8080/`] : []),
      ...gateways.slice(0, 1).map((g) => `ws://${g}:80/`),
      'ws://portquiz.net:8080/',
    ];
    const out = await chromiumWebSockets(browser, urls);
    for (const r of out)
      record('H', r.url.replace(/^wss?:\/\//, '').slice(0, 22), 'chromium-ws', r.url, 'refuse', {
        outcome: r.state === 'open' ? 'open' : 'ws-failed',
        detail: `${r.state}${r.code ? ` code ${r.code}` : ''}`,
      });
  }

  // I — the VPS's own public addresses
  if (want('I')) {
    for (const [i, ip] of selfIps.entries()) {
      const host = ip.includes(':') ? `[${ip}]` : ip;
      const tag = `self-${i}`;
      await raw('I', `${tag}-http`, `GET http://${host}/`);
      await raw('I', `${tag}-https`, `CONNECT ${host}:443`);
      await raw('I', `${tag}-ssh`, `CONNECT ${host}:22`);
      await nav('I', `${tag}-nav`, `http://${host}/`);
      if (!ip.includes(':')) await raw('I', `${tag}-nip`, `CONNECT ${ip}.nip.io:443`);
    }
  }

  // J — direct egress that bypasses the proxy
  if (want('J')) {
    for (const [id, host, port] of [
      ['tcp-cloudflare-443', '1.1.1.1', 443],
      ['tcp-google-dns-53', '8.8.8.8', 53],
      ['tcp-example-80', 'example.com', 80],
      ...[...selfIps.entries()]
        .filter(([, ip]) => !ip.includes(':'))
        .map(([i, ip]) => [`tcp-self-${i}-443`, ip, 443]),
    ])
      record('J', id, 'tcp', `${host}:${port}`, 'refuse', await tcpConnect(host, port));
    record('J', 'udp-dns-8.8.8.8', 'udp', '8.8.8.8:53', 'refuse', await udpDnsQuery('8.8.8.8'));
    try {
      const a = await dnsPromises.resolve4('example.com');
      record(
        'J',
        'docker-dns-external',
        'dns',
        'example.com via the container resolver',
        'refuse',
        { outcome: 'resolved', detail: `${a.length} A records` },
      );
    } catch (e) {
      record(
        'J',
        'docker-dns-external',
        'dns',
        'example.com via the container resolver',
        'refuse',
        { outcome: 'not-resolved', detail: e.code || e.message },
      );
    }
    // The host's addresses on the Docker bridges (sshd, exporters, the Docker API…).
    for (const [i, g] of gateways.entries())
      for (const port of [22, 53, 80, 443, 2375, 4750, 8080, 9100])
        record(
          'J',
          `gw-${i}-${port}`,
          'tcp',
          `${g}:${port}`,
          'refuse',
          await tcpConnect(g, port, 3000),
        );
    if (canaryIps[0])
      record(
        'J',
        'lateral-canary',
        'tcp',
        `${canaryIps[0]}:80 (peer on the capture network)`,
        'info',
        await tcpConnect(canaryIps[0], 80),
      );
    if (browser) {
      const cands = await chromiumWebRtc(browser);
      const srflx = cands.filter((c) => /typ srflx/.test(c));
      record('J', 'webrtc-stun', 'webrtc', 'stun:stun.l.google.com:19302', 'refuse', {
        outcome: srflx.length ? 'connected' : 'no-srflx',
        detail: `${cands.length} candidates, ${srflx.length} server-reflexive`,
      });
      // Chromium's implicit loopback bypass. The capture launch must not reach
      // a listener on the container's own loopback; a raw --proxy-server flag does.
      let hits = 0;
      const srv = http.createServer((req, res) => {
        hits++;
        res.end('reached');
      });
      await new Promise((r) => srv.listen(18081, '127.0.0.1', r));
      for (const u of [
        'http://127.0.0.1:18081/',
        'http://localhost:18081/',
        'http://[::1]:18081/',
      ]) {
        const before = hits;
        const r = await chromiumNav(browser, u, 10_000);
        record(
          'J',
          `loopback-${new URL(u).hostname.replace(/[[\]]/g, '')}`,
          'chromium-nav',
          u,
          'refuse',
          hits > before
            ? { outcome: 'reached', detail: 'the local listener saw the request' }
            : { ...r, outcome: REFUSED.has(r.outcome) ? r.outcome : 'not-reached' },
        );
      }
      const bad = await launchChromium(pw, proxy, { rawProxyFlag: true });
      const before = hits;
      const r = await chromiumNav(bad, 'http://127.0.0.1:18081/', 10_000);
      record(
        'J',
        'loopback-control',
        'chromium-nav',
        'http://127.0.0.1:18081/ with a raw --proxy-server flag',
        'info',
        hits > before
          ? { outcome: 'reached', detail: 'expected: the raw flag bypasses the proxy for loopback' }
          : { outcome: r.outcome, detail: r.detail },
      );
      await bad.close();
      srv.close();
    }
  }

  // K — DNS rebinding: names that answer 127.0.0.1 or 1.1.1.1 from one query to
  // the next (rbndr.us at random, 1u.ms round-robin). Reaching 1.1.1.1 is fine;
  // reaching 127.0.0.1 is not: the analysis checks the address the proxy
  // actually connected to, in its log.
  if (want('K')) {
    for (const [tag, host] of [
      ['rbndr', '7f000001.01010101.rbndr.us'],
      ['1u-ms', 'make-1-1-1-1-rebind-127-0-0-1-rr.1u.ms'],
    ])
      for (let i = 0; i < 8; i++) {
        const r = await rawProxy(proxy, `GET http://${host}/`);
        record('K', `${tag}-${i}`, 'raw-http', `http://${host}/`, 'info', {
          ...classifyProxy(r),
          trace: r.trace,
        });
      }
  }

  // L — chaining through another proxy chosen by the client
  if (want('L')) {
    await raw('L', 'upstream-header', 'CONNECT example.com:443', 'refuse', {
      'X-Upstream-Https-Proxy': 'https://10.0.0.1:3128',
    });
    await raw('L', 'upstream-public', 'CONNECT example.com:443', 'refuse', {
      'X-Upstream-Https-Proxy': 'https://example.net:443',
    });
  }

  if (browser) await browser.close();
  return { results, sandbox };
}

// ─── Canary ──────────────────────────────────────────────────────────────────

function runCanary(opts) {
  const ports = list(opts.ports || '80,443,8080').map(Number);
  const udp = list(opts.udp || '53').map(Number);
  const log = (o) =>
    process.stdout.write(`${JSON.stringify({ t: new Date().toISOString(), ...o })}\n`);
  for (const port of ports) {
    net
      .createServer((s) => {
        log({ proto: 'tcp', port, from: s.remoteAddress?.replace(/^::ffff:/, '') });
        s.end('HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncanary');
      })
      .on('error', (e) => log({ proto: 'tcp', port, error: e.code }))
      .listen(port, '0.0.0.0', () => log({ listening: `tcp/${port}` }));
  }
  for (const port of udp) {
    const s = dgram.createSocket('udp4');
    s.on('message', (_m, rinfo) => log({ proto: 'udp', port, from: rinfo.address }));
    s.on('error', (e) => log({ proto: 'udp', port, error: e.code }));
    s.bind(port, '0.0.0.0', () => log({ listening: `udp/${port}` }));
  }
}

// ─── Sandbox-only mode ───────────────────────────────────────────────────────

async function runSandbox(opts) {
  const pw = loadPlaywright();
  if (!pw) throw new Error('playwright-core not found next to this script');
  const t0 = Date.now();
  let browser;
  try {
    browser = await launchChromium(pw, opts.proxy || 'http://127.0.0.1:9');
  } catch (e) {
    const msg = String(e?.message || e);
    const lines = msg
      .split('\n')
      .filter((l) =>
        /sandbox|namespace|seccomp|clone|zygote|EPERM|Operation not permitted|FATAL|No usable/i.test(
          l,
        ),
      );
    process.stdout.write(
      `${JSON.stringify({ launched: false, error: lines.slice(0, 12).join(' | ').slice(0, 1500) || msg.slice(0, 600) })}\n`,
    );
    process.exitCode = 2;
    return;
  }
  const { context, page } = await newProbePage(browser);
  await page
    .evaluate(() => {
      const c = document.createElement('canvas');
      return !!c.getContext('webgl');
    })
    .catch(() => false);
  await new Promise((r) => setTimeout(r, 1500));
  const report = sandboxReport();
  const webgl = await page
    .evaluate(() => {
      const c = document.createElement('canvas');
      const gl = c.getContext('webgl');
      return gl ? gl.getParameter(gl.VERSION) : null;
    })
    .catch(() => null);
  // Chromium's own view of the GPU process (SwiftShader runs WebGL there).
  let gpu = null;
  try {
    const cdp = await browser.newBrowserCDPSession();
    const info = await cdp.send('SystemInfo.getInfo');
    const aux = info.gpu?.auxAttributes || {};
    gpu = { sandboxed: aux.sandboxed ?? null, gl: aux.glImplementationParts ?? null };
  } catch {}
  await context.close();
  const version = browser.version();
  await browser.close();
  process.stdout.write(
    `${JSON.stringify({ launched: true, ms: Date.now() - t0, version, webgl, gpu, sandboxEnv: process.env.SHELFY_DISABLE_SANDBOX === '1' ? 'off' : 'on', processes: report })}\n`,
  );
}

// ─── Analysis ────────────────────────────────────────────────────────────────

function readJsonLines(file) {
  if (!file || !fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, 'utf8')
    .split('\n')
    .map((l) => {
      const i = l.indexOf('{');
      if (i < 0) return null;
      try {
        return JSON.parse(l.slice(i));
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

function analyze(opts) {
  const data = JSON.parse(fs.readFileSync(opts.results, 'utf8'));
  const proxyIps = list(opts['proxy-ips']);
  const selfIps = list(opts['self-ips']);
  const proxyLog = readJsonLines(opts['proxy-log']);
  const canaryLog = readJsonLines(opts['canary-log']);

  // Every connection the proxy opened, with the address it reached.
  const decisions = new Map();
  for (const e of proxyLog) {
    if (e.msg === 'CANONICAL-PROXY-DECISION') decisions.set(e.id, { ...e });
    if (e.msg === 'CANONICAL-PROXY-CN-CLOSE' && decisions.has(e.id))
      Object.assign(decisions.get(e.id), { outbound_remote_addr: e.outbound_remote_addr });
  }
  const allowed = [...decisions.values()].filter((d) => d.allow);
  const leaks = allowed.filter((d) => {
    const addr = String(d.outbound_remote_addr || '')
      .replace(/:\d+$/, '')
      .replace(/^\[|\]$/g, '');
    return addr && !isPublicAddress(addr, selfIps);
  });
  const ports = allowed
    .map((d) =>
      Number(
        String(d.outbound_remote_addr || '')
          .split(':')
          .pop(),
      ),
    )
    .filter(Boolean);
  const badPorts = ports.filter((p) => p !== 80 && p !== 443);
  const rebinding = allowed.filter((d) => /rbndr\.us|1u\.ms/.test(d.requested_host || ''));
  const canaryHits = canaryLog.filter((e) => e.proto);
  const viaProxy = canaryHits.filter((h) => proxyIps.includes(h.from));

  const rows = data.results;
  const byCat = {};
  for (const r of rows) {
    byCat[r.cat] ||= { pass: 0, FAIL: 0, info: 0 };
    byCat[r.cat][r.verdict]++;
  }
  const fails = rows.filter((r) => r.verdict === 'FAIL');
  const summary = {
    probes: rows.length,
    pass: rows.filter((r) => r.verdict === 'pass').length,
    fail: fails.length,
    info: rows.filter((r) => r.verdict === 'info').length,
    byCat,
    proxyDecisions: decisions.size,
    proxyAllowed: allowed.length,
    proxyAllowedToNonPublic: leaks.map((d) => ({
      host: d.requested_host,
      addr: d.outbound_remote_addr,
    })),
    proxyAllowedNon80or443: badPorts.length,
    rebindingAllowed: rebinding.map((d) => mask(d.outbound_remote_addr || '', selfIps)),
    canaryHits: canaryHits.map((h) => ({
      proto: h.proto,
      port: h.port,
      from: proxyIps.includes(h.from) ? 'proxy' : h.from,
    })),
    canaryHitsViaProxy: viaProxy.length,
    sandbox: data.sandbox,
  };
  const ok =
    summary.fail === 0 && leaks.length === 0 && badPorts.length === 0 && viaProxy.length === 0;
  summary.verdict = ok ? 'PASS' : 'FAIL';
  if (opts.markdown) {
    const lines = [
      '| Cat | Probe | Via | Target | Outcome | Detail | Verdict |',
      '|---|---|---|---|---|---|---|',
    ];
    for (const r of rows)
      lines.push(
        `| ${r.cat} | ${r.id} | ${r.via} | \`${r.target.replace(/\|/g, '\\|').slice(0, 70)}\` | ${r.outcome} | ${String(r.detail).replace(/\|/g, '\\|').slice(0, 90)} | ${r.verdict} |`,
      );
    process.stdout.write(`${lines.join('\n')}\n\n`);
  }
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
  if (!ok) process.exitCode = 1;
}

// ─── Main ────────────────────────────────────────────────────────────────────

const opts = parseArgs(process.argv.slice(2));
if (opts.mode === 'canary') runCanary(opts);
else if (opts.mode === 'sandbox') await runSandbox(opts);
else if (opts.mode === 'analyze') analyze(opts);
else if (opts.mode === 'probe') {
  const t0 = Date.now();
  const out = await runProbes(opts);
  const data = {
    at: new Date().toISOString(),
    seconds: Math.round((Date.now() - t0) / 1000),
    node: process.version,
    ...out,
  };
  if (opts.out) fs.writeFileSync(opts.out, JSON.stringify(data, null, 2));
  const fails = out.results.filter((r) => r.verdict === 'FAIL').length;
  process.stdout.write(`\n${out.results.length} probes, ${fails} FAIL\n`);
  process.exit(fails ? 1 : 0);
} else throw new Error(`unknown mode ${opts.mode}`);
