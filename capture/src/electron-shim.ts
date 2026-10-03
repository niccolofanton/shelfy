// Build-time stand-in for the `electron` module (capture/build.ts aliases
// `electron` to this file). The capture pipeline (electron/webcap/*) is written
// for Electron; in the service there is no Electron, so capture v2's only needs
// from it — app.getPath / getLocale / isPackaged — are served from the
// environment, and the Electron-only fallbacks (session, BrowserWindow) throw so
// a Playwright launch failure fails the capture instead of falling back
// (SPIKE-11). electron/ is never edited for this (D25-friendly).

import os from 'os';
import path from 'path';

function unavailable(what: string): never {
  throw new Error(`${what} is not available in the capture service (no Electron)`);
}

// One work dir per capture: the server sets CAPTURE_WORK_DIR to /work/<captureId>
// before each run, so <userData>/assets/web resolves inside that dir.
function workDir(): string {
  return process.env.CAPTURE_WORK_DIR || path.join(os.tmpdir(), 'shelfy-capture');
}

export const app = {
  isPackaged: true,
  getPath(name: string): string {
    if (name === 'userData' || name === 'appData') return workDir();
    if (name === 'temp') return os.tmpdir();
    if (name === 'home') return os.homedir();
    return unavailable(`app.getPath('${name}')`);
  },
  getLocale(): string {
    return process.env.CAPTURE_LOCALE || 'en-US';
  },
  getVersion(): string {
    return process.env.CAPTURE_VERSION || 'capture-service';
  },
};

export const session = {
  fromPartition(): never {
    return unavailable('session.fromPartition');
  },
  defaultSession: {
    get cookies(): never {
      return unavailable('session.defaultSession.cookies');
    },
  },
};

export class BrowserWindow {
  constructor() {
    unavailable('BrowserWindow');
  }
  static getAllWindows(): BrowserWindow[] {
    return [];
  }
}

export default { app, session, BrowserWindow };
