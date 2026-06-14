'use strict';

// Build prep: download the Chromium browser the Playwright capture engine needs
// into build/ms-playwright, so electron-builder can ship it as extraResources
// (→ resources/ms-playwright in the packaged app). The runtime resolver
// webcapture-playwright.ensureBrowsersPath() points PLAYWRIGHT_BROWSERS_PATH there
// when packaged.
//
// Per-platform: the build runs once on each OS (see .github/workflows/release.yml), so this
// installs the Chromium build for the CURRENT platform only — exactly what that
// platform's package needs. Idempotent: skips the (slow) download when a chromium
// build is already present.
//
// Run automatically before electron-builder via the `build`/`release` npm scripts,
// or manually: node build/prepare-playwright.cjs

import { execFileSync } from 'child_process';
import fs from 'fs';
import path from 'path';

const ROOT = path.resolve(__dirname, '..');
const DEST = path.join(ROOT, 'build', 'ms-playwright');

interface BrowserEntry {
  name: string;
  revision?: string;
}

// The EXACT chromium-headless-shell build revision playwright-core pins for this
// install. The on-disk layout is <dest>/chromium_headless_shell-<rev>/, and the
// executable path the engine resolves at runtime is keyed to this revision, so the
// idempotency check below must match THIS revision (not any headless-shell dir).
function pinnedHeadlessShellRevision(): string | undefined {
  const { browsers } = JSON.parse(
    fs.readFileSync(path.join(ROOT, 'node_modules', 'playwright-core', 'browsers.json'), 'utf8'),
  ) as { browsers: BrowserEntry[] };
  const entry = browsers.find((b) => b.name === 'chromium-headless-shell');
  return entry && entry.revision;
}

function hasChromium(): boolean {
  try {
    if (!fs.existsSync(DEST)) return false;
    const rev = pinnedHeadlessShellRevision();
    if (!rev) return false;
    // We bundle ONLY the headless shell (the engine launches with headless:true →
    // chrome-headless-shell), laid out as <dest>/chromium_headless_shell-<rev>/.
    // Common case: the pinned revision dir, fully installed (INSTALLATION_COMPLETE
    // marker — playwright-core writes it only on a validated install, so an
    // interrupted download isn't treated as present).
    if (fs.existsSync(path.join(DEST, `chromium_headless_shell-${rev}`, 'INSTALLATION_COMPLETE'))) {
      return true;
    }
    // Platforms with a revisionOverrides entry resolve to a DIFFERENT on-disk
    // revision/dir than `revision` (which pinnedHeadlessShellRevision doesn't
    // resolve), so also accept any fully-installed headless-shell dir already in
    // DEST — otherwise those hosts would re-download every run. The install step
    // below still pins the version via the repo-local playwright-core CLI.
    return fs
      .readdirSync(DEST)
      .some(
        (d) =>
          /^chromium_headless_shell-/.test(d) &&
          fs.existsSync(path.join(DEST, d, 'INSTALLATION_COMPLETE')),
      );
  } catch {
    return false;
  }
}

function main(): void {
  if (hasChromium()) {
    console.log(`[prepare-playwright] chromium already present in ${DEST} — skipping download`);
    return;
  }
  fs.mkdirSync(DEST, { recursive: true });
  console.log(`[prepare-playwright] installing chromium-headless-shell into ${DEST} …`);
  // Install ONLY the headless shell (~90MB) rather than the full chromium (~500MB
  // with both builds): the engine launches headless, so the shell is all it needs.
  // `playwright install` honors PLAYWRIGHT_BROWSERS_PATH for the install location;
  // use the repo-local CLI so the version matches package.json's playwright-core.
  execFileSync(
    process.execPath,
    [
      path.join(ROOT, 'node_modules', 'playwright-core', 'cli.js'),
      'install',
      'chromium-headless-shell',
    ],
    {
      stdio: 'inherit',
      env: { ...process.env, PLAYWRIGHT_BROWSERS_PATH: DEST },
    },
  );
  console.log('[prepare-playwright] done');
}

main();
