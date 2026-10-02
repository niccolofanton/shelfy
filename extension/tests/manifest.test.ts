import { describe, expect, it } from 'vitest';
import { SOCIAL_MATCHES } from '../src/hosts';
import {
  FILES,
  MIN_CHROME_VERSION,
  buildManifest,
  isValidMatchPattern,
  validateBundles,
  validateManifest,
  type ChromeManifest,
} from '../src/manifest';

const allFiles = new Set<string>(Object.values(FILES));
const exists = (path: string): boolean => allFiles.has(path);

const goodBundles: Record<string, string> = {
  [FILES.hook]:
    '(() => { const MSG_TYPE = "SOCIAL_SAVED_INTERCEPT"; window.__ssReplayPinterest = 1; window.__ssScanTwitterBookmarks = 1; })();',
  [FILES.bridge]: '(() => { window.addEventListener("message", () => {}); })();',
  [FILES.serviceWorker]: 'chrome.runtime.onMessage.addListener(() => false);',
  [FILES.panelScript]: '(() => {})();',
  [FILES.panelHtml]: '<link rel="stylesheet" href="panel.css" /><script src="panel.js"></script>',
};
const read = (files: Record<string, string>) => (path: string) => files[path] ?? null;

function mutated(change: (m: ChromeManifest) => void): ChromeManifest {
  const manifest = structuredClone(buildManifest());
  change(manifest);
  return manifest;
}

describe('buildManifest', () => {
  it('passes its own sanity check', () => {
    expect(validateManifest(buildManifest(), exists)).toEqual([]);
  });

  it('has the plan §2.16 essentials for passive capture', () => {
    const manifest = buildManifest();
    expect(manifest.manifest_version).toBe(3);
    expect(Number(manifest.minimum_chrome_version)).toBe(MIN_CHROME_VERSION);
    expect(manifest.background).toEqual({ service_worker: 'sw.js', type: 'module' });
    expect(manifest.content_scripts[0]).toMatchObject({
      js: ['hook.main.js'],
      world: 'MAIN',
      run_at: 'document_start',
    });
    expect(manifest.content_scripts[1]).toMatchObject({
      js: ['bridge.js'],
      run_at: 'document_start',
    });
    expect(manifest.content_scripts[1].world).toBeUndefined();
    expect(manifest.host_permissions).toEqual([...SOCIAL_MATCHES]);
    expect(manifest.side_panel.default_path).toBe('panel.html');
  });
});

describe('validateManifest', () => {
  it('reports missing files', () => {
    const problems = validateManifest(buildManifest(), (path) => path !== FILES.serviceWorker);
    expect(problems).toEqual(['service worker file is missing: sw.js']);
  });

  it('refuses a hook outside the MAIN world or after document_start', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.content_scripts[0].world = 'ISOLATED';
        m.content_scripts[0].run_at = 'document_idle';
      }),
      exists,
    );
    expect(problems).toEqual([
      'the hook must run in the MAIN world',
      'the hook must run at document_start',
    ]);
  });

  it('refuses permissions and hosts the spike does not need', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.permissions.push('debugger');
        m.host_permissions.push('<all_urls>', 'https://*.cdninstagram.com/*');
        m.externally_connectable = { matches: ['https://example.com/*'] };
      }),
      exists,
    );
    expect(problems).toEqual([
      'permission "debugger" is outside the spike\'s allowlist',
      '"externally_connectable" must not be set in the spike build',
      'host permission "<all_urls>" is not a valid match pattern',
      'host permission "<all_urls>" is not a supported social host',
      'host permission "https://*.cdninstagram.com/*" is not a supported social host',
    ]);
  });

  it('refuses content scripts without host permission, in subframes, or out of sync', () => {
    const problems = validateManifest(
      mutated((m) => {
        m.host_permissions = m.host_permissions.filter((p) => p !== 'https://x.com/*');
        m.content_scripts[1].all_frames = true;
        m.content_scripts[1].matches = m.content_scripts[1].matches.slice(1);
      }),
      exists,
    );
    expect(problems).toContain('content_scripts[1] must stay in the top frame');
    expect(problems).toContain(
      'content script match "https://x.com/*" has no host permission (needed for the IG replay)',
    );
    expect(problems).toContain('hook and bridge must match the same pages');
  });

  it('validates version strings and match patterns', () => {
    expect(
      validateManifest(
        mutated((m) => (m.version = '1.2.3.4.5')),
        exists,
      ),
    ).toEqual(['version "1.2.3.4.5" is not 1-4 dot-separated integers']);
    expect(isValidMatchPattern('https://*.pinterest.co.uk/*')).toBe(true);
    expect(isValidMatchPattern('https://www.instagram.com/*')).toBe(true);
    expect(isValidMatchPattern('https://www.instagram.com')).toBe(false);
    expect(isValidMatchPattern('ftp://example.com/*')).toBe(false);
    expect(isValidMatchPattern('https://*foo.com/*')).toBe(false);
  });
});

describe('validateBundles', () => {
  it('accepts bundles that contain the desktop hook and no eval or imports', () => {
    expect(validateBundles(read(goodBundles))).toEqual([]);
  });

  it('flags a hook without the desktop markers, eval, runtime imports and inline scripts', () => {
    const problems = validateBundles(
      read({
        ...goodBundles,
        [FILES.hook]: '(() => {})();',
        [FILES.bridge]: 'eval("1")',
        [FILES.serviceWorker]: 'import { x } from "./x.js";',
        [FILES.panelScript]: 'async function igFeedReplay(o) { __name(post, "post"); }',
        [FILES.panelHtml]:
          '<script>alert(1)</script><script src="https://cdn.example.com/x.js"></script>',
      }),
    );
    expect(problems).toEqual([
      'hook.main.js does not contain "SOCIAL_SAVED_INTERCEPT"',
      'hook.main.js does not contain "__ssReplayPinterest"',
      'hook.main.js does not contain "__ssScanTwitterBookmarks"',
      'bridge.js uses eval/new Function',
      'sw.js still has a runtime import',
      'panel.js contains __name() (keepNames)',
      'panel.html does not reference panel.js',
      'panel.html does not reference panel.css',
      'panel.html has an inline script',
      'panel.html loads a remote resource',
    ]);
  });
});
