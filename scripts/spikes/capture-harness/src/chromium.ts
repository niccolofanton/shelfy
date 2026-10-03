// Chromium for the capture service, without touching electron/: capture v2
// launches its browser in electron/webcapture-playwright.ts with fixed options,
// so the harness wraps playwright-core's chromium.launch (the same module
// instance that file requires) to add the egress proxy, the egress flags and the
// offline fixture sites. P4's env.ts should make these launch options
// configurable instead.

import fs from 'fs';
import path from 'path';
import { chromium } from 'playwright-core';
import type { Browser, LaunchOptions, Route } from 'playwright-core';

export const FIXTURE_ORIGIN = 'https://fixtures.shelfy.test';

// Plan §2.18, Egress row. QUIC never goes through an HTTP proxy, and WebRTC may
// only use proxied TCP: no UDP to peers on the capture network, no STUN.
export const EGRESS_ARGS = [
  '--disable-quic',
  '--force-webrtc-ip-handling-policy=disable_non_proxied_udp',
];

export interface LaunchSettings {
  proxy: string | null; // e.g. http://shelfy-egress:4750; null only for local runs
  fixtureDir: string | null; // serves FIXTURE_ORIGIN/<file>.html from this directory
}

export interface EffectiveLaunch {
  launches: number;
  headless?: boolean;
  chromiumSandbox?: boolean;
  proxy: string | null;
  args: string[];
}

export const effectiveLaunch: EffectiveLaunch = { launches: 0, proxy: null, args: [] };

export function patchChromium(settings: LaunchSettings): void {
  const original = chromium.launch.bind(chromium);
  chromium.launch = async (opts: LaunchOptions = {}): Promise<Browser> => {
    const args = [...(opts.args || []), ...EGRESS_ARGS];
    // Playwright's `proxy` option (not a raw --proxy-server flag) also sets
    // --proxy-bypass-list=<-loopback>: without it Chromium connects to
    // loopback addresses directly, bypassing the proxy (SPIKE-4 probe L1).
    const options: LaunchOptions = {
      ...opts,
      args,
      ...(settings.proxy ? { proxy: { server: settings.proxy } } : {}),
    };
    effectiveLaunch.launches++;
    effectiveLaunch.headless = options.headless;
    effectiveLaunch.chromiumSandbox = options.chromiumSandbox;
    effectiveLaunch.proxy = settings.proxy;
    effectiveLaunch.args = args;
    const browser = await original(options);
    if (settings.fixtureDir) serveFixtures(browser, settings.fixtureDir);
    return browser;
  };
}

// Fixture pages are answered by Playwright's router, before any DNS lookup or
// proxy connection: they cost no network and need no allow rule in the proxy.
// Capture v2 registers its own catch-all route after this one; it runs first
// and falls back to this handler for fixture URLs.
function serveFixtures(browser: Browser, dir: string): void {
  const newContext = browser.newContext.bind(browser);
  browser.newContext = async (...a: Parameters<Browser['newContext']>) => {
    const context = await newContext(...a);
    await context.route(`${FIXTURE_ORIGIN}/**`, (route) => fulfillFixture(route, dir));
    return context;
  };
}

async function fulfillFixture(route: Route, dir: string): Promise<void> {
  const name = path.basename(new URL(route.request().url()).pathname);
  const file = path.join(dir, name);
  if (!/^[\w.-]+\.html$/.test(name) || !fs.existsSync(file)) {
    await route.fulfill({ status: 404, contentType: 'text/plain', body: 'not found' });
    return;
  }
  await route.fulfill({
    status: 200,
    contentType: 'text/html; charset=utf-8',
    body: fs.readFileSync(file),
  });
}

export interface SandboxProcess {
  type: string; // browser, zygote, renderer, gpu-process, utility:<service>
  count: number;
  // Each count below is the number of processes of this type that differ from
  // the browser process, i.e. that Chromium's own sandbox put there.
  ownUserNs: number; // /proc/<pid>/ns/user
  ownPidNs: number; // /proc/<pid>/ns/pid
  ownNetNs: number; // /proc/<pid>/ns/net
  extraSeccomp: number; // more seccomp filters than the browser (Docker's is the first)
}

interface ProcInfo {
  type: string;
  filters: number;
  ns: Record<'user' | 'pid' | 'net', string>;
}

function chromiumProcesses(): ProcInfo[] {
  const out: ProcInfo[] = [];
  for (const pid of fs.readdirSync('/proc').filter((d) => /^\d+$/.test(d))) {
    let cmd: string;
    let status: string;
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
    const ns = { user: '', pid: '', net: '' };
    for (const k of ['user', 'pid', 'net'] as const) {
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

// The sandbox layers of every Chromium process, read from /proc (Linux only).
export function sandboxReport(): SandboxProcess[] {
  if (process.platform !== 'linux') return [];
  const procs = chromiumProcesses();
  const base = procs.find((p) => p.type === 'browser');
  if (!base) return [];
  const byType = new Map<string, SandboxProcess>();
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
