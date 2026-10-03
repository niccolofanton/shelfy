import { describe, it, expect, vi } from 'vitest';

// The capture graph imports `electron`; in the service it is the env-backed shim.
vi.mock('electron', () => import('../src/electron-shim'));

const { configureCapture, ENV } = await import('../src/env');
const { getBrowserLaunchOverrides } = await import('../../electron/webcap/browser');
const { captureSite } = await import('../../electron/webcap/capture');
const shim = await import('../src/electron-shim');

describe('configureCapture — Chromium launch and egress flags', () => {
  it('injects the proxy, the egress flags, sandbox on and no self-install', () => {
    configureCapture({ ...ENV, proxy: 'http://shelfy-egress:4750' });
    const o = getBrowserLaunchOverrides();
    expect(o.proxy).toEqual({ server: 'http://shelfy-egress:4750' });
    expect(o.extraArgs).toContain('--disable-quic');
    expect(o.extraArgs).toContain('--force-webrtc-ip-handling-policy=disable_non_proxied_udp');
    expect(o.chromiumSandbox).toBe(true);
    expect(o.selfHeal).toBe(false);
    expect(o.headless).toBe(true);
    // Node fetch is routed through the same proxy.
    expect(process.env.NODE_USE_ENV_PROXY).toBe('1');
    expect(process.env.HTTP_PROXY).toBe('http://shelfy-egress:4750');
    expect(process.env.HTTPS_PROXY).toBe('http://shelfy-egress:4750');
  });
});

describe('the Electron fallbacks cannot run in the service', () => {
  it('session and BrowserWindow throw (electron-driver / system-chrome cannot start)', () => {
    expect(() => new shim.BrowserWindow()).toThrow(/not available/);
    expect(() => shim.session.fromPartition()).toThrow(/not available/);
  });

  it('a session-launch failure fails the capture — no fallback on the server', async () => {
    await expect(
      captureSite('https://example.com/', {
        maxPages: 1,
        singlePage: true,
        stamp: 1,
        video: false,
        pagesParallel: 1,
        deviceScale: 1,
        // The service passes NO fallbackSession, so a launch failure propagates.
        createSession: () => Promise.reject(new Error('Executable does not exist')),
      }),
    ).rejects.toThrow(/Executable does not exist/);
  });
});
