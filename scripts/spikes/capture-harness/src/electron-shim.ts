// Stand-in for the `electron` module, so capture v2 (electron/webcap/*) runs in
// plain Node. build.mjs aliases `electron` to this file; electron/ stays untouched
// (D25). It is the prototype of the capture service's env.ts (plan §2.18).
//
// What capture v2 needs from Electron, and what it gets here:
//   app.getPath('userData')  → CAPTURE_WORK_DIR: assets land in <work>/assets/web
//   app.getLocale()          → CAPTURE_LOCALE (default en-US): Accept-Language, context locale
//   app.isPackaged = true    → take the packaged paths: the adblock engine from
//                              <process.resourcesPath>/adblock/engine.bin, and no
//                              runtime download of Chromium (the image ships it)
//   session, BrowserWindow   → only the Electron fallback driver and the v1 engine
//                              use them; both throw here, so a Playwright launch
//                              failure ends the capture instead of falling back.

import os from 'os';
import path from 'path';

function unavailable(what: string): never {
  throw new Error(`${what} is not available in the capture service (no Electron)`);
}

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
    return 'capture-harness';
  },
};

export const session = {
  fromPartition(): never {
    return unavailable('session.fromPartition');
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
