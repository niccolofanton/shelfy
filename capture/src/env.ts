// Environment and configuration for the capture service (plan §2.18, SPIKE-11).
//
// Reads the operator env, applies the injectable knobs the desktop refactor
// exposed (browser launch options + idle close, ffmpeg threads), and holds the
// per-run budgets. The capture runs 1 page at a time at 1× scale, behind the
// egress proxy, with the server budgets (SPIKE-11 / lead decision L18).

import { setBrowserLaunchOverrides, setBrowserIdleTimeout } from '../../electron/webcap/browser';
import { setFfmpegThreads } from '../../electron/webcap/encode';

function int(name: string, fallback: number): number {
  const v = Number(process.env[name]);
  return Number.isFinite(v) && v > 0 ? Math.floor(v) : fallback;
}

export interface CaptureEnv {
  port: number;
  internalToken: string | null;
  proxy: string | null;
  fake: boolean;
  fixturesDir: string | null;
  // Physical root for work dirs. Production: /work (the shared mount), so the
  // physical dir equals the request's logical workDir (/work/<captureId>). Tests
  // point it at a temp dir while the request still carries /work/<captureId>.
  workBase: string;
  // Budgets (SPIKE-11 / L18). All overridable by env for the VPS measurements.
  slots: number;
  pagesParallel: number;
  deviceScale: number;
  encodePoolSize: number;
  primaryDeadlineMs: number;
  innerDeadlineMs: number;
  siteBudgetMs: number;
  maxArtifactBytes: number;
  idleMs: number;
  maxLineBytes: number;
  maxEvents: number;
  // O6 (open): the server scroll video records 1–2.5 fps without a GPU. Kept
  // behind this setting, OFF by default, until the owner decides (SPIKE-11 #4).
  video: boolean;
}

export const ENV: CaptureEnv = {
  port: int('PORT', 8080),
  internalToken: process.env.SHELFY_INTERNAL_TOKEN || null,
  proxy: process.env.CAPTURE_PROXY || null,
  fake: process.env.SHELFY_CAPTURE_FAKE === '1',
  fixturesDir: process.env.CAPTURE_FIXTURES || null,
  workBase: process.env.CAPTURE_WORK_BASE || '/work',
  slots: int('CAPTURE_SLOTS', 1), // 1 site at a time (plan §2.18)
  pagesParallel: int('CAPTURE_PAGES_PARALLEL', 1), // 1 page at a time (SPIKE-11)
  deviceScale: int('CAPTURE_DEVICE_SCALE', 1), // 1× on the server (SPIKE-11)
  encodePoolSize: int('CAPTURE_ENCODE_POOL', 2), // 1–2 is enough at 1.5 CPU (SPIKE-11 #6)
  primaryDeadlineMs: int('CAPTURE_PRIMARY_DEADLINE_MS', 180_000), // 180 s primary page
  innerDeadlineMs: int('CAPTURE_INNER_DEADLINE_MS', 150_000), // 150 s inner pages
  siteBudgetMs: int('CAPTURE_SITE_BUDGET_MS', 12 * 60_000), // 12 min site budget (keeps finished pages)
  maxArtifactBytes: int('CAPTURE_MAX_ARTIFACT_BYTES', 80 * 1024 * 1024), // ≤ 80 MB per site
  idleMs: int('CAPTURE_IDLE_MS', 120_000), // close the browser after 120 s idle
  maxLineBytes: int('CAPTURE_MAX_LINE_BYTES', 256 * 1024), // NDJSON line cap
  maxEvents: int('CAPTURE_MAX_EVENTS', 250), // at most 250 events per capture
  // Default OFF (O6): the slideshow scroll video is not worth the memory peak on
  // a GPU-less box until the owner decides. SHELFY_CAPTURE_VIDEO=1 turns it on.
  video: process.env.SHELFY_CAPTURE_VIDEO === '1',
};

let configured = false;

// Apply the env to the shared capture modules. Called once at service start.
export function configureCapture(env: CaptureEnv = ENV): void {
  if (configured) return;
  configured = true;

  // Node's built-in fetch (discovery, sitemaps, og:image, favicon) goes through
  // the egress proxy via NODE_USE_ENV_PROXY + HTTP(S)_PROXY (SPIKE-11).
  if (env.proxy) {
    process.env.HTTP_PROXY = env.proxy;
    process.env.HTTPS_PROXY = env.proxy;
    process.env.NODE_USE_ENV_PROXY = '1';
  }

  // Chromium: Playwright's `proxy` option (NOT --proxy-server, L18), the egress
  // flags, sandbox on (unless SHELFY_DISABLE_SANDBOX), no runtime self-install.
  setBrowserLaunchOverrides({
    proxy: env.proxy ? { server: env.proxy } : undefined,
    extraArgs: [
      '--disable-quic',
      // WebRTC may only use proxied TCP: no UDP to peers, no STUN leak.
      '--force-webrtc-ip-handling-policy=disable_non_proxied_udp',
    ],
    chromiumSandbox: process.env.SHELFY_DISABLE_SANDBOX !== '1',
    headless: true,
    selfHeal: false, // the image ships chromium-headless-shell; never download at runtime
    executablePath: process.env.CAPTURE_CHROMIUM_PATH || undefined,
    channel: process.env.CAPTURE_CHROMIUM_CHANNEL || undefined,
  });
  setBrowserIdleTimeout(env.idleMs);

  // ffmpeg -threads 1 so an encode never competes with Chromium for the vCPUs.
  setFfmpegThreads(1);

  if (!process.env.CAPTURE_VERSION) process.env.CAPTURE_VERSION = 'capture-service';
}
