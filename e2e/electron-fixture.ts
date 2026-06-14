import { test as base, _electron as electron, expect } from '@playwright/test';
import type { ElectronApplication, Page } from '@playwright/test';
import path from 'path';
import { fileURLToPath } from 'url';
import { MOCK_POSTS, MOCK_STATS, MOCK_DOWNLOAD_JOBS } from './test-data';

const __filename = fileURLToPath(import.meta.url);
const __dirname  = path.dirname(__filename);
const ROOT       = path.join(__dirname, '..');

type Fixtures = {
  electronApp: ElectronApplication;
  page: Page;
};

// Installs mock IPC handlers in the Electron main process.
// Called after firstWindow() so the real handlers are already registered
// and can be replaced.
export async function installMocks(app: ElectronApplication) {
  await app.evaluate(({ ipcMain }, { posts, stats, jobs }) => {
    const channels = [
      'db:getPosts', 'db:getStats', 'db:importJSON', 'db:bulkUpsert',
      'download:post', 'download:all', 'download:status', 'download:cancel',
      'dialog:openFile', 'shell:openPath', 'shell:openExternal',
    ];
    channels.forEach((ch) => {
      try { ipcMain.removeHandler(ch); } catch { /* handler may not exist yet */ }
    });

    ipcMain.handle('db:getPosts', (_event, filters: any) => {
      let result = posts.slice();
      if (filters?.platform)  result = result.filter((p: any) => p.platform  === filters.platform);
      if (filters?.mediaType) result = result.filter((p: any) => p.mediaType === filters.mediaType);
      if (filters?.search) {
        const q = (filters.search as string).toLowerCase();
        result = result.filter((p: any) =>
          ((p.text as string) || '').toLowerCase().includes(q) ||
          ((p.authorUsername as string) || '').toLowerCase().includes(q),
        );
      }
      const offset = filters?.offset ?? 0;
      const limit  = filters?.limit  ?? 50;
      // Mirror the real db.getPosts contract: usePosts reads result.posts /
      // result.total, so returning a bare array makes posts undefined and
      // crashes the Gallery render.
      return { posts: result.slice(offset, offset + limit), total: result.length };
    });

    ipcMain.handle('db:getStats',  () => stats);
    ipcMain.handle('db:importJSON', () => ({ imported: 7 }));
    ipcMain.handle('db:bulkUpsert', () => 0);

    ipcMain.handle('download:post',   () => ({ thumbnailPath: '/mock/thumb.jpg' }));
    ipcMain.handle('download:all',    () => []);
    ipcMain.handle('download:status', () => jobs);
    ipcMain.handle('download:cancel', () => null);

    ipcMain.handle('dialog:openFile',    () => '/Users/test/bookmarks.json');
    ipcMain.handle('shell:openPath',     () => null);
    ipcMain.handle('shell:openExternal', () => null);
  }, { posts: MOCK_POSTS, stats: MOCK_STATS, jobs: MOCK_DOWNLOAD_JOBS });
}

// Override a single IPC handler in the main process from within a test.
export async function overrideHandler(
  app: ElectronApplication,
  channel: string,
  returnValue: unknown,
) {
  await app.evaluate(({ ipcMain }, { ch, val }) => {
    try { ipcMain.removeHandler(ch); } catch { /* ok */ }
    ipcMain.handle(ch, () => val);
  }, { ch: channel, val: returnValue });
}

export const test = base.extend<Fixtures>({
  electronApp: async ({}, use) => {
    const app = await electron.launch({
      args: [path.join(ROOT, 'electron', 'main.js')],
      cwd: ROOT,
      env: {
        ...process.env,
        ELECTRON_DEV: 'true',
        NODE_ENV: 'test',
        PLAYWRIGHT_E2E: '1', // disables DevTools auto-open so Playwright keeps the right page reference
      },
    });
    await use(app);
    await app.close();
  },

  page: async ({ electronApp }, use) => {
    const p = await electronApp.firstWindow();
    // Wait for the initial page to be in a stable state
    await p.waitForLoadState('domcontentloaded');
    // Replace real IPC handlers with deterministic mocks
    await installMocks(electronApp);
    // Navigate to the dev server URL fresh — more reliable than page.reload() in Electron
    await p.goto('http://localhost:5173', { waitUntil: 'domcontentloaded' });
    // The sidebar is our "app is ready" signal
    await p.waitForSelector('[data-testid="sidebar"]', { timeout: 20_000 });
    await use(p);
  },
});

export { expect };
export type { ElectronApplication, Page };
