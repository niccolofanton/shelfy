// manifest.json generator and the build's sanity checks (plan §2.16, P2-06). Shared registry S7:
// a lane that needs a new permission, host or file changes it here, where the sanity check pins
// exactly this set.
//
// - Permissions: storage, unlimitedStorage, alarms, scripting, sidePanel, notifications (P2-G9:
//   the daily "Sync now" reminder, P2-15). Nothing else, no optional permissions.
// - Hosts: the social hosts (content scripts and the IG replay), the platform CDNs (extension
//   uploads, P2-17) and the Shelfy origin of the build (the API, without CORS).
// - `externally_connectable`: the Shelfy origin only (C9).
// - `key`: the committed public key that pins the extension's ID (P2-G6, src/id.ts).

import { EXTENSION_PUBLIC_KEY } from './id';
import {
  CDN_MATCHES,
  DEFAULT_SHELFY_ORIGIN,
  PINTEREST_HOSTS,
  SOCIAL_MATCHES,
  originMatch,
} from './shared/hosts';
import { CENSUS_MESSAGE, INTERCEPT_MESSAGE } from './shared/protocol';
import { EXTENSION_VERSION } from './shared/version';

/** MAIN-world content scripts need Chrome 111, the side panel 114; the plan pins 120. */
export const MIN_CHROME_VERSION = 120;

export const FILES = {
  hook: 'hook.main.js',
  bridge: 'bridge.js',
  select: 'select.main.js',
  serviceWorker: 'sw.js',
  panelScript: 'panel.js',
  panelHtml: 'panel.html',
  panelCss: 'panel.css',
  icon: 'icon.png',
} as const;

/** Exactly the permissions the extension holds; the sanity check refuses any difference. */
export const PERMISSIONS = [
  'storage',
  'unlimitedStorage',
  'alarms',
  'scripting',
  'sidePanel',
  'notifications',
] as const;

/** Storage key of the extension token (sw/settings.ts KEYS.pairing): never in a content script. */
export const TOKEN_STORAGE_KEY = 'shelfy.pairing';

export interface BuildOptions {
  /** The Shelfy origin the build talks to (validated by parseShelfyOrigin). */
  origin: string;
  debug: boolean;
}

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
  key?: string;
  permissions: string[];
  host_permissions: string[];
  background: { service_worker: string; type?: 'module' };
  content_scripts: ContentScript[];
  externally_connectable?: { matches?: string[]; ids?: string[]; [key: string]: unknown };
  side_panel: { default_path: string };
  action: { default_title: string; default_icon?: Record<string, string> };
  icons?: Record<string, string>;
  [key: string]: unknown;
}

/** host_permissions of a build: social hosts, CDN hosts, the Shelfy origin. */
export function expectedHostPermissions(origin: string): string[] {
  return [...SOCIAL_MATCHES, ...CDN_MATCHES, originMatch(origin)];
}

/** "0.2.0", or "0.2.0 localhost:18286 debug" for builds that are not the production one. */
export function versionName(options: BuildOptions): string {
  const parts: string[] = [EXTENSION_VERSION];
  if (options.origin !== DEFAULT_SHELFY_ORIGIN) parts.push(new URL(options.origin).host);
  if (options.debug) parts.push('debug');
  return parts.join(' ');
}

export function buildManifest(options: BuildOptions): ChromeManifest {
  const matches = (): string[] => [...SOCIAL_MATCHES];
  return {
    manifest_version: 3,
    name: 'Shelfy',
    short_name: 'Shelfy',
    description:
      'Saves the posts you bookmark on Instagram, X and Pinterest into your private Shelfy library.',
    version: EXTENSION_VERSION,
    version_name: versionName(options),
    minimum_chrome_version: String(MIN_CHROME_VERSION),
    key: EXTENSION_PUBLIC_KEY,
    permissions: [...PERMISSIONS],
    host_permissions: expectedHostPermissions(options.origin),
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
    externally_connectable: { matches: [originMatch(options.origin)] },
    side_panel: { default_path: FILES.panelHtml },
    action: { default_title: 'Open Shelfy', default_icon: { 128: FILES.icon } },
    icons: { 128: FILES.icon },
  };
}

/** Chrome match pattern grammar, restricted to http(s) schemes, with an optional port. */
export function isValidMatchPattern(pattern: string): boolean {
  return /^(?:https?|\*):\/\/(?:\*|(?:\*\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*)(?::(?:\d{1,5}|\*))?\/.*$/.test(
    pattern,
  );
}

const sameSet = (a: readonly string[], b: readonly string[]): boolean =>
  a.length === b.length && new Set(a).size === a.length && b.every((item) => a.includes(item));

const FORBIDDEN_KEYS = [
  'content_security_policy',
  'web_accessible_resources',
  'optional_permissions',
  'optional_host_permissions',
  'update_url',
  'oauth2',
  'sandbox',
] as const;

const SOCIAL_HOSTS = new Set([
  'www.instagram.com',
  'x.com',
  'twitter.com',
  ...PINTEREST_HOSTS.map((host) => `*.${host}`),
]);

function patternHost(pattern: string): string {
  return pattern.replace(/^[^:]+:\/\//, '').replace(/\/.*$/, '');
}

/** Returns every problem found; an empty array means the manifest is loadable and in scope. */
export function validateManifest(
  manifest: ChromeManifest,
  options: BuildOptions,
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
  need(manifest.name === 'Shelfy', 'name must be "Shelfy"');
  need(manifest.description.length <= 132, 'description must be at most 132 characters');
  need(manifest.version === EXTENSION_VERSION, `version must be ${EXTENSION_VERSION}`);
  need(
    /^\d{1,5}(?:\.\d{1,5}){0,3}$/.test(manifest.version) &&
      manifest.version.split('.').every((part) => Number(part) <= 65535),
    `version "${manifest.version}" is not 1-4 dot-separated integers`,
  );
  need(
    Number(manifest.minimum_chrome_version) >= MIN_CHROME_VERSION,
    `minimum_chrome_version must be >= ${MIN_CHROME_VERSION}`,
  );
  need(manifest.key === EXTENSION_PUBLIC_KEY, 'key must be the committed public key (src/id.ts)');

  need(
    sameSet(manifest.permissions, PERMISSIONS),
    `permissions must be exactly ${PERMISSIONS.join(', ')}`,
  );
  for (const forbidden of FORBIDDEN_KEYS)
    need(!(forbidden in manifest), `"${forbidden}" must not be set`);

  const hosts = expectedHostPermissions(options.origin);
  need(
    sameSet(manifest.host_permissions, hosts),
    'host_permissions must be exactly the social hosts, the CDN hosts and the Shelfy origin',
  );
  for (const pattern of manifest.host_permissions)
    need(isValidMatchPattern(pattern), `host permission "${pattern}" is not a valid match pattern`);

  const connectable = manifest.externally_connectable;
  need(
    !!connectable &&
      Object.keys(connectable).length === 1 &&
      JSON.stringify(connectable.matches) === JSON.stringify([originMatch(options.origin)]),
    `externally_connectable must be exactly { matches: ["${originMatch(options.origin)}"] }`,
  );

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
        SOCIAL_HOSTS.has(patternHost(pattern)),
        `content script match "${pattern}" is not a supported social host`,
      );
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
export function validateBundles(
  readFile: (relativePath: string) => string | null,
  options: Pick<BuildOptions, 'debug'>,
): string[] {
  const problems: string[] = [];
  const hook = readFile(FILES.hook) ?? '';
  // Markers proving electron/webview-injected.ts was bundled into the MAIN-world script.
  for (const marker of [
    INTERCEPT_MESSAGE,
    '__ssReplayPinterest',
    '__ssScanTwitterBookmarks',
    '__ssEmitInstagramRest',
  ])
    if (!hook.includes(marker)) problems.push(`${FILES.hook} does not contain "${marker}"`);
  // The request census is in debug builds only.
  if (options.debug && !hook.includes(CENSUS_MESSAGE))
    problems.push(`${FILES.hook} of a debug build has no request census`);
  if (!options.debug && hook.includes(CENSUS_MESSAGE))
    problems.push(`${FILES.hook} of a release build contains the request census`);

  for (const file of [
    FILES.hook,
    FILES.bridge,
    FILES.select,
    FILES.serviceWorker,
    FILES.panelScript,
  ]) {
    const code = readFile(file);
    if (code === null) {
      problems.push(`${file} is missing`);
      continue;
    }
    // MV3 forbids eval-like code, and the hook runs inside the page.
    if (/\beval\s*\(|\bnew\s+Function\s*\(/.test(code))
      problems.push(`${file} uses eval/new Function`);
    // No remote or late-loaded code: everything is in the bundle.
    if (/^\s*import\s|\bimport\s*\(|\bimportScripts\s*\(/m.test(code))
      problems.push(`${file} loads code at runtime (import/importScripts)`);
    // esbuild's keepNames helper: chrome.scripting serializes functions with toString(), and
    // a __name() call inside one would throw a ReferenceError in the page.
    if (/\b__name\s*\(/.test(code)) problems.push(`${file} contains __name() (keepNames)`);
  }
  // The token never reaches page JS or a content script (P2-06): their bundles cannot even
  // name its storage key or the Authorization header.
  for (const file of [FILES.hook, FILES.bridge, FILES.select]) {
    const code = readFile(file) ?? '';
    if (code.includes(TOKEN_STORAGE_KEY) || /\bAuthorization\b/.test(code))
      problems.push(`${file} references the extension token`);
  }
  const html = readFile(FILES.panelHtml) ?? '';
  for (const asset of [FILES.panelScript, FILES.panelCss])
    if (!html.includes(`"${asset}"`))
      problems.push(`${FILES.panelHtml} does not reference ${asset}`);
  if (/<script(?![^>]*\bsrc=)[^>]*>/i.test(html))
    problems.push(`${FILES.panelHtml} has an inline script`);
  if (/\son[a-z]+\s*=/i.test(html)) problems.push(`${FILES.panelHtml} has an inline event handler`);
  if (/\b(?:src|href)="(?:https?:)?\/\//i.test(html))
    problems.push(`${FILES.panelHtml} loads a remote resource`);
  return problems;
}
