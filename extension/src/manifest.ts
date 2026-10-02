// manifest.json generator and the build's sanity checks. The manifest follows plan §2.16,
// trimmed to what passive capture needs (see docs/web-port/spikes/03-extension-capture.md):
// no CDN hosts, no Shelfy origin, no externally_connectable and no alarms yet.

import { PINTEREST_HOSTS, SOCIAL_MATCHES } from './hosts';

export const EXTENSION_VERSION = '0.1.0';
export const EXTENSION_VERSION_NAME = '0.1.0-spike3';
/** MAIN-world content scripts need Chrome 111, the side panel 114; the plan pins 120. */
export const MIN_CHROME_VERSION = 120;

export const FILES = {
  hook: 'hook.main.js',
  bridge: 'bridge.js',
  serviceWorker: 'sw.js',
  panelScript: 'panel.js',
  panelHtml: 'panel.html',
  panelCss: 'panel.css',
  icon: 'icon.png',
} as const;

/** Permissions the spike may request; anything else fails the sanity check. */
export const ALLOWED_PERMISSIONS = [
  'storage',
  'unlimitedStorage',
  'scripting',
  'sidePanel',
] as const;

export interface ContentScript {
  matches: string[];
  js: string[];
  run_at: 'document_start' | 'document_end' | 'document_idle';
  world?: 'MAIN' | 'ISOLATED';
  all_frames?: boolean;
}

export interface ChromeManifest {
  manifest_version: number;
  name: string;
  short_name?: string;
  description: string;
  version: string;
  version_name?: string;
  minimum_chrome_version: string;
  permissions: string[];
  host_permissions: string[];
  background: { service_worker: string; type?: 'module' };
  content_scripts: ContentScript[];
  side_panel: { default_path: string };
  action: { default_title: string; default_icon?: Record<string, string> };
  icons?: Record<string, string>;
  [key: string]: unknown;
}

export function buildManifest(): ChromeManifest {
  const matches = (): string[] => [...SOCIAL_MATCHES];
  return {
    manifest_version: 3,
    name: 'Shelfy capture spike (SPIKE-3)',
    short_name: 'Shelfy spike',
    description:
      'Developer build for SPIKE-3: records the saved items Instagram, X and Pinterest load in this browser.',
    version: EXTENSION_VERSION,
    version_name: EXTENSION_VERSION_NAME,
    minimum_chrome_version: String(MIN_CHROME_VERSION),
    permissions: [...ALLOWED_PERMISSIONS],
    host_permissions: matches(),
    background: { service_worker: FILES.serviceWorker, type: 'module' },
    content_scripts: [
      {
        matches: matches(),
        js: [FILES.hook],
        run_at: 'document_start',
        world: 'MAIN',
        all_frames: false,
      },
      { matches: matches(), js: [FILES.bridge], run_at: 'document_start', all_frames: false },
    ],
    side_panel: { default_path: FILES.panelHtml },
    action: {
      default_title: 'Open the Shelfy capture spike panel',
      default_icon: { 128: FILES.icon },
    },
    icons: { 128: FILES.icon },
  };
}

/** Chrome match pattern grammar, restricted to http(s) schemes. */
export function isValidMatchPattern(pattern: string): boolean {
  return /^(?:https?|\*):\/\/(?:\*|(?:\*\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*)\/.*$/.test(pattern);
}

function patternHost(pattern: string): string {
  return pattern.replace(/^[^:]+:\/\//, '').replace(/\/.*$/, '');
}

const SOCIAL_HOST_PATTERNS = new Set([
  'www.instagram.com',
  'x.com',
  'twitter.com',
  ...PINTEREST_HOSTS.map((host) => `*.${host}`),
]);

/** Returns every problem found; an empty array means the manifest is loadable and in scope. */
export function validateManifest(
  manifest: ChromeManifest,
  fileExists: (relativePath: string) => boolean,
): string[] {
  const problems: string[] = [];
  const need = (condition: boolean, problem: string): void => {
    if (!condition) problems.push(problem);
  };
  const needFile = (path: string | undefined, what: string): void => {
    need(!!path && fileExists(path), `${what} file is missing: ${path ?? '(none)'}`);
  };

  need(manifest.manifest_version === 3, 'manifest_version must be 3');
  need(manifest.name.length > 0 && manifest.name.length <= 75, 'name must be 1-75 characters');
  need(manifest.description.length <= 132, 'description must be at most 132 characters');
  need(
    /^\d{1,5}(?:\.\d{1,5}){0,3}$/.test(manifest.version) &&
      manifest.version.split('.').every((part) => Number(part) <= 65535),
    `version "${manifest.version}" is not 1-4 dot-separated integers`,
  );
  need(
    Number(manifest.minimum_chrome_version) >= MIN_CHROME_VERSION,
    `minimum_chrome_version must be >= ${MIN_CHROME_VERSION}`,
  );

  for (const permission of manifest.permissions)
    need(
      (ALLOWED_PERMISSIONS as readonly string[]).includes(permission),
      `permission "${permission}" is outside the spike's allowlist`,
    );
  for (const forbidden of [
    'content_security_policy',
    'web_accessible_resources',
    'externally_connectable',
  ])
    need(!(forbidden in manifest), `"${forbidden}" must not be set in the spike build`);

  for (const pattern of manifest.host_permissions) {
    need(isValidMatchPattern(pattern), `host permission "${pattern}" is not a valid match pattern`);
    need(
      SOCIAL_HOST_PATTERNS.has(patternHost(pattern)),
      `host permission "${pattern}" is not a supported social host`,
    );
  }

  need(manifest.background.type === 'module', 'background.type must be "module"');
  needFile(manifest.background.service_worker, 'service worker');
  needFile(manifest.side_panel.default_path, 'side panel');
  for (const icon of Object.values({ ...manifest.icons, ...manifest.action.default_icon }))
    needFile(icon, 'icon');

  const scripts = manifest.content_scripts;
  need(scripts.length === 2, 'expected exactly two content scripts (MAIN hook + ISOLATED bridge)');
  const [hook, bridge] = scripts;
  if (hook) {
    need(
      hook.js.length === 1 && hook.js[0] === FILES.hook,
      `content_scripts[0] must be ${FILES.hook}`,
    );
    need(hook.world === 'MAIN', 'the hook must run in the MAIN world');
    need(hook.run_at === 'document_start', 'the hook must run at document_start');
  }
  if (bridge) {
    need(
      bridge.js.length === 1 && bridge.js[0] === FILES.bridge,
      `content_scripts[1] must be ${FILES.bridge}`,
    );
    need(bridge.world === undefined || bridge.world === 'ISOLATED', 'the bridge must run ISOLATED');
    need(bridge.run_at === 'document_start', 'the bridge must run at document_start');
  }
  for (const [index, script] of scripts.entries()) {
    need(script.all_frames !== true, `content_scripts[${index}] must stay in the top frame`);
    for (const file of script.js) needFile(file, `content_scripts[${index}]`);
    for (const pattern of script.matches) {
      need(isValidMatchPattern(pattern), `content script match "${pattern}" is invalid`);
      need(
        manifest.host_permissions.includes(pattern),
        `content script match "${pattern}" has no host permission (needed for the IG replay)`,
      );
    }
  }
  if (hook && bridge)
    need(
      JSON.stringify(hook.matches) === JSON.stringify(bridge.matches),
      'hook and bridge must match the same pages',
    );
  return problems;
}

/** Checks on the built files themselves (read relative to the dist folder). */
export function validateBundles(readFile: (relativePath: string) => string | null): string[] {
  const problems: string[] = [];
  const hook = readFile(FILES.hook) ?? '';
  // Markers proving electron/webview-injected.ts was bundled into the MAIN-world script.
  for (const marker of [
    'SOCIAL_SAVED_INTERCEPT',
    '__ssReplayPinterest',
    '__ssScanTwitterBookmarks',
  ])
    if (!hook.includes(marker)) problems.push(`${FILES.hook} does not contain "${marker}"`);
  for (const file of [FILES.hook, FILES.bridge, FILES.serviceWorker, FILES.panelScript]) {
    const code = readFile(file);
    if (code === null) {
      problems.push(`${file} is missing`);
      continue;
    }
    // MV3 forbids eval-like code, and the hook runs inside the page.
    if (/\beval\s*\(|\bnew\s+Function\s*\(/.test(code))
      problems.push(`${file} uses eval/new Function`);
    if (/^\s*import\s|\bimport\s*\(/m.test(code))
      problems.push(`${file} still has a runtime import`);
    // esbuild's keepNames helper: chrome.scripting serializes igFeedReplay with toString(), and
    // a __name() call inside it would throw a ReferenceError in the page.
    if (/\b__name\s*\(/.test(code)) problems.push(`${file} contains __name() (keepNames)`);
  }
  const html = readFile(FILES.panelHtml) ?? '';
  for (const asset of [FILES.panelScript, FILES.panelCss])
    if (!html.includes(`"${asset}"`))
      problems.push(`${FILES.panelHtml} does not reference ${asset}`);
  if (/<script(?![^>]*\bsrc=)[^>]*>/i.test(html))
    problems.push(`${FILES.panelHtml} has an inline script`);
  if (/\bsrc="https?:/i.test(html)) problems.push(`${FILES.panelHtml} loads a remote resource`);
  return problems;
}
