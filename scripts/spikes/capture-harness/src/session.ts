// A capture session with a cap on inner pages captured at once and, for cost
// comparisons only, another device scale. capture v2 runs
// min(session.maxConcurrency ?? 3, WebGL or scroll-jacked ? 2 : 3) inner pages
// in parallel, but PlaywrightSession.create() takes no cap, so this repeats its
// context setup with one. P4's env.ts needs the same knob (plan §2.18: 2 pages,
// 1 when WebGL-heavy). capture v2 reads the scale back from the band PNGs, so a
// 1× session still produces consistent crops, at half the resolution.

import { getBrowser } from '../../../../electron/webcapture-playwright';
import {
  PlaywrightSession,
  configureContext,
  desktopUserAgent,
  acceptLanguage,
  VIEWPORT,
} from '../../../../electron/webcap/driver';

export async function createSession(
  maxConcurrency: number | undefined,
  deviceScaleFactor: number = VIEWPORT.scale,
): Promise<PlaywrightSession> {
  const browser = await getBrowser();
  const userAgent = desktopUserAgent(browser.version());
  // Same options as PlaywrightSession.create() in electron/webcap/driver.ts.
  const context = await browser.newContext({
    viewport: { width: VIEWPORT.width, height: VIEWPORT.height },
    deviceScaleFactor,
    userAgent,
    locale: process.env.CAPTURE_LOCALE || 'en-US',
    extraHTTPHeaders: { 'Accept-Language': acceptLanguage() },
    serviceWorkers: 'block',
    bypassCSP: true,
    ignoreHTTPSErrors: false,
    acceptDownloads: false,
  });
  await configureContext(context);
  return new PlaywrightSession(context, userAgent, false, maxConcurrency);
}
