'use strict';

// Ensure the Playwright Chromium (headless shell) the web-capture engine needs is
// present in the DEFAULT ms-playwright cache after every `npm install`, so a dev
// checkout can capture WebGL sites without a manual extra step.
//
// Best-effort: a download failure (offline, proxy, CI without network) must NEVER
// fail `npm install` — the runtime self-heal in webcapture-playwright.js
// (ensureChromium) downloads it on first capture as a fallback. Idempotent:
// `playwright install` skips an already-present browser.

const { execFileSync } = require('child_process');
const path = require('path');

try {
  const cli = path.join(__dirname, '..', 'node_modules', 'playwright-core', 'cli.js');
  execFileSync(process.execPath, [cli, 'install', 'chromium-headless-shell'], {
    stdio: 'inherit',
    env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
  });
} catch (err) {
  console.warn(
    '[postinstall] could not pre-install Chromium headless shell (will self-heal at runtime):',
    err && err.message ? err.message : err,
  );
  // Exit 0 regardless — never break the install.
}
