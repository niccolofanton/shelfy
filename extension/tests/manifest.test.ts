import { describe, expect, it } from 'vitest';
import { EXTENSION_PUBLIC_KEY } from '../src/id';
import {
  FILES,
  MIN_CHROME_VERSION,
  PERMISSIONS,
  TOKEN_STORAGE_KEY,
  buildManifest,
  expectedHostPermissions,
  isValidMatchPattern,
  validateBundles,
  validateManifest,
  versionName,
  type BuildOptions,
  type ChromeManifest,
} from '../src/manifest';
import { CDN_MATCHES, DEFAULT_SHELFY_ORIGIN, SOCIAL_MATCHES } from '../src/shared/hosts';
import { KEYS } from '../src/sw/settings';

const PROD: BuildOptions = { origin: DEFAULT_SHELFY_ORIGIN, debug: false };
const DEV: BuildOptions = { origin: 'http://localhost:18286', debug: true };

const allFiles = new Set<string>(Object.values(FILES));
const exists = (path: string): boolean => allFiles.has(path);

const goodBundles: Record<string, string> = {
  [FILES.hook]:
    '(() => { const MSG_TYPE = "SOCIAL_SAVED_INTERCEPT"; window.__ssReplayPinterest = 1; window.__ssScanTwitterBookmarks = 1; })();',
  [FILES.select]: '(() => { window.__ssSelect = {}; })();',
  [FILES.bridge]: '(() => { window.addEventListener("message", () => {}); })();',
  [FILES.serviceWorker]: 'chrome.runtime.onMessage.addListener(() => false);',
  [FILES.panelScript]: '(() => {})();',
  [FILES.panelHtml]: '<link rel="stylesheet" href="panel.css" /><script src="panel.js"></script>',
};
const read = (files: Record<string, string>) => (path: string) => files[path] ?? null;

function mutated(change: (m: ChromeManifest) => void, options = PROD): ChromeManifest {
  const manifest = structuredClone(buildManifest(options));
  change(manifest);
  return manifest;
}

describe('buildManifest', () => {
  it('passes its own sanity check, for production and development origins', () => {
    expect(validateManifest(buildManifest(PROD), PROD, exists)).toEqual([]);
    expect(validateManifest(buildManifest(DEV), DEV, exists)).toEqual([]);
  });

  it('has the P2-06 essentials: name, version, Chrome 120, the key, exactly six permissions', () => {
    const manifest = buildManifest(PROD);
    expect(manifest).toMatchObject({
      manifest_version: 3,
      name: 'Shelfy',
      version: '0.2.0',
      version_name: '0.2.0',
      key: EXTENSION_PUBLIC_KEY,
    });
    expect(Number(manifest.minimum_chrome_version)).toBe(MIN_CHROME_VERSION);
    expect([...manifest.permissions].sort()).toEqual(
      ['alarms', 'notifications', 'scripting', 'sidePanel', 'storage', 'unlimitedStorage'].sort(),
    );
    expect(manifest.description.length).toBeLessThanOrEqual(132);
  });

  it('hosts: the social hosts, the CDN hosts and the Shelfy origin; externally_connectable: the origin', () => {
    const manifest = buildManifest(PROD);
    expect(manifest.host_permissions).toEqual([
      ...SOCIAL_MATCHES,
      ...CDN_MATCHES,
      'https://refs.niccolofanton.dev/*',
    ]);
    expect(manifest.externally_connectable).toEqual({
      matches: ['https://refs.niccolofanton.dev/*'],
    });
    const dev = buildManifest(DEV);
    expect(dev.host_permissions.at(-1)).toBe('http://localhost:18286/*');
    expect(dev.externally_connectable).toEqual({ matches: ['http://localhost:18286/*'] });
    expect(dev.version_name).toBe('0.2.0 localhost:18286 debug');
    expect(versionName({ origin: DEFAULT_SHELFY_ORIGIN, debug: true })).toBe('0.2.0 debug');
  });

  it('runs the hook in the MAIN world and the bridge isolated, at document_start, top frame', () => {
    const manifest = buildManifest(PROD);
    expect(manifest.background).toEqual({ service_worker: 'sw.js', type: 'module' });
    expect(manifest.content_scripts[0]).toMatchObject({
      js: ['hook.main.js'],
      world: 'MAIN',
      run_at: 'document_start',
      matches: [...SOCIAL_MATCHES],
    });
    expect(manifest.content_scripts[1]).toMatchObject({
      js: ['bridge.js'],
      run_at: 'document_start',
      matches: [...SOCIAL_MATCHES],
    });
    expect(manifest.content_scripts[1].world).toBeUndefined();
    expect(manifest.side_panel.default_path).toBe('panel.html');
  });

  it('names the token storage key the worker really uses', () => {
    expect(TOKEN_STORAGE_KEY).toBe(KEYS.pairing);
  });
});

describe('validateManifest', () => {
  it('reports missing files', () => {
    const problems = validateManifest(
      buildManifest(PROD),
      PROD,
      (path) => path !== FILES.serviceWorker,
    );
    expect(problems).toEqual(['service worker file is missing: sw.js']);
  });

  it('enforces exactly the permission set: no extra, none missing, no optional ones', () => {
    const extra = validateManifest(
      mutated((m) => m.permissions.push('debugger')),
      PROD,
      exists,
    );
    expect(extra).toEqual([`permissions must be exactly ${PERMISSIONS.join(', ')}`]);
    const missing = validateManifest(
      mutated((m) => (m.permissions = m.permissions.filter((p) => p !== 'notifications'))),
      PROD,
      exists,
    );
    expect(missing).toEqual([`permissions must be exactly ${PERMISSIONS.join(', ')}`]);
    const duplicated = validateManifest(
      mutated((m) => (m.permissions = [...m.permissions.slice(1), m.permissions[1]])),
      PROD,
      exists,
    );
    expect(duplicated).toHaveLength(1);
    const optional = validateManifest(
      mutated((m) => (m.optional_permissions = ['tabs'])),
      PROD,
      exists,
    );
    expect(optional).toEqual(['"optional_permissions" must not be set']);
  });

  it('refuses other hosts, another origin, a wider externally_connectable and a missing key', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.host_permissions.push('<all_urls>');
        m.externally_connectable = { matches: ['https://example.com/*'], ids: ['*'] };
        delete m.key;
        m.web_accessible_resources = [];
      }),
      PROD,
      exists,
    );
    expect(problems).toEqual([
      'key must be the committed public key (src/id.ts)',
      '"web_accessible_resources" must not be set',
      'host_permissions must be exactly the social hosts, the CDN hosts and the Shelfy origin',
      'host permission "<all_urls>" is not a valid match pattern',
      'externally_connectable must be exactly { matches: ["https://refs.niccolofanton.dev/*"] }',
    ]);
    // A production manifest checked against a development build's origin.
    expect(validateManifest(buildManifest(PROD), DEV, exists)).toContain(
      'host_permissions must be exactly the social hosts, the CDN hosts and the Shelfy origin',
    );
    expect(expectedHostPermissions(DEV.origin)).toContain('http://localhost:18286/*');
  });

  it('refuses a hook outside the MAIN world or after document_start', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.content_scripts[0].world = 'ISOLATED';
        m.content_scripts[0].run_at = 'document_idle';
      }),
      PROD,
      exists,
    );
    expect(problems).toEqual([
      'the hook must run in the MAIN world',
      'the hook must run at document_start',
    ]);
  });

  it('refuses content scripts on other hosts, without host permission, in subframes, out of sync', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.host_permissions = m.host_permissions.filter((p) => p !== 'https://x.com/*');
        m.content_scripts[1].all_frames = true;
        m.content_scripts[1].matches = m.content_scripts[1].matches.slice(1);
        m.content_scripts[0].matches.push('https://*.cdninstagram.com/*');
      }),
      PROD,
      exists,
    );
    expect(problems).toContain('content_scripts[1] must stay in the top frame');
    expect(problems).toContain(
      'content script match "https://x.com/*" has no host permission (needed for the IG replay)',
    );
    expect(problems).toContain(
      'content script match "https://*.cdninstagram.com/*" is not a supported social host',
    );
    expect(problems).toContain('hook and bridge must match the same pages');
  });

  it('validates name, version strings and match patterns (with ports)', () => {
    expect(
      validateManifest(
        mutated((m) => (m.version = '1.2.3.4.5')),
        PROD,
        exists,
      ),
    ).toEqual(['version must be 0.2.0', 'version "1.2.3.4.5" is not 1-4 dot-separated integers']);
    expect(
      validateManifest(
        mutated((m) => (m.name = 'Shelfy spike')),
        PROD,
        exists,
      ),
    ).toEqual(['name must be "Shelfy"']);
    expect(isValidMatchPattern('https://*.pinterest.co.uk/*')).toBe(true);
    expect(isValidMatchPattern('https://www.instagram.com/*')).toBe(true);
    expect(isValidMatchPattern('http://localhost:18286/*')).toBe(true);
    expect(isValidMatchPattern('http://localhost:*/*')).toBe(true);
    expect(isValidMatchPattern('https://www.instagram.com')).toBe(false);
    expect(isValidMatchPattern('ftp://example.com/*')).toBe(false);
    expect(isValidMatchPattern('https://*foo.com/*')).toBe(false);
  });
});

describe('validateBundles', () => {
  it('accepts release bundles with the desktop hook, no census, no eval or imports', () => {
    expect(validateBundles(read(goodBundles), { debug: false })).toEqual([]);
  });

  it('wants the census in debug builds and refuses it in release builds', () => {
    const withCensus = {
      ...goodBundles,
      [FILES.hook]: `${goodBundles[FILES.hook]} postMessage({ type: "SHELFY_CENSUS" });`,
    };
    expect(validateBundles(read(withCensus), { debug: true })).toEqual([]);
    expect(validateBundles(read(withCensus), { debug: false })).toEqual([
      'hook.main.js of a release build contains the request census',
    ]);
    expect(validateBundles(read(goodBundles), { debug: true })).toEqual([
      'hook.main.js of a debug build has no request census',
    ]);
  });

  it('flags missing markers, eval, runtime and remote code, inline scripts and token leaks', () => {
    const problems = validateBundles(
      read({
        ...goodBundles,
        [FILES.hook]: '(() => { chrome.storage.local.get("shelfy.pairing"); })();',
        [FILES.bridge]: 'eval("1"); fetch(u, { headers: { Authorization: t } });',
        [FILES.serviceWorker]: 'importScripts("https://cdn.example.com/x.js");',
        [FILES.panelScript]: 'async function igFeedReplay(o) { __name(post, "post"); }',
        [FILES.panelHtml]:
          '<script>alert(1)</script><script src="https://cdn.example.com/x.js"></script><button onclick="x()">',
      }),
      { debug: false },
    );
    expect(problems).toEqual([
      'hook.main.js does not contain "SOCIAL_SAVED_INTERCEPT"',
      'hook.main.js does not contain "__ssReplayPinterest"',
      'hook.main.js does not contain "__ssScanTwitterBookmarks"',
      'bridge.js uses eval/new Function',
      'sw.js loads code at runtime (import/importScripts)',
      'panel.js contains __name() (keepNames)',
      'hook.main.js references the extension token',
      'bridge.js references the extension token',
      'panel.html does not reference panel.js',
      'panel.html does not reference panel.css',
      'panel.html has an inline script',
      'panel.html has an inline event handler',
      'panel.html loads a remote resource',
    ]);
  });
});
