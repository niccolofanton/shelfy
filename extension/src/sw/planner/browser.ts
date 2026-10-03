// Chrome adapter. Account/session reads execute in the page's MAIN world;
// platform cookies and usernames are never sent to the Shelfy API.
import { MSG, type BridgePong, type Platform } from '../../shared/protocol';
import type { PlannerBrowser } from './service';

function bounded<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('bridge_timeout')), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
// Both functions are serialized by chrome.scripting: no outer references.
async function currentInstagramUser(): Promise<{ username: string | null; login: boolean }> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15_000);
  try {
    const response = await fetch('/api/v1/accounts/current_user/', {
      credentials: 'same-origin',
      headers: { 'X-IG-App-ID': '936619743392459' },
      signal: controller.signal,
    });
    if (response.status === 401 || response.status === 403) return { username: null, login: true };
    if (!response.ok) return { username: null, login: false };
    const data = await response.json();
    const username: unknown = data?.user?.username;
    return {
      username:
        typeof username === 'string' && /^[A-Za-z0-9._]{1,64}$/.test(username) ? username : null,
      login: false,
    };
  } catch {
    return { username: null, login: false };
  } finally {
    clearTimeout(timer);
  }
}
function instagramFolderLinks(): string[] {
  return [...document.querySelectorAll<HTMLAnchorElement>('a[href]')]
    .map((link) => link.href)
    .filter((href) => /^https:\/\/www\.instagram\.com\/[^/?#]+\/saved\/[^/?#]+\/\d+\/$/.test(href))
    .slice(0, 2000);
}
export function createPlannerBrowser(): PlannerBrowser {
  async function visibleWindow(id: number, minimized = false): Promise<void> {
    await chrome.windows.update(id, {
      state: minimized ? 'minimized' : 'normal',
      focused: !minimized,
    });
  }
  return {
    async open(_platform: Platform, previousWindowId, minimized) {
      if (previousWindowId != null) {
        const window = await chrome.windows
          .get(previousWindowId, { populate: true })
          .catch(() => null);
        const tab = window?.type === 'normal' ? window.tabs?.[0] : null;
        if (tab?.id != null) {
          await visibleWindow(previousWindowId, minimized);
          await chrome.tabs.update(tab.id, { active: true });
          return { windowId: previousWindowId, tabId: tab.id };
        }
      }
      const window = await chrome.windows.create({
        url: 'about:blank',
        type: 'normal',
        state: minimized ? 'minimized' : 'normal',
        focused: !minimized,
      });
      const tabId = window.tabs?.[0]?.id;
      if (window.id == null || tabId == null) throw new Error('window_unavailable');
      return { windowId: window.id, tabId };
    },
    async existing(tabId) {
      const tab = await chrome.tabs.get(tabId);
      await visibleWindow(tab.windowId);
      await chrome.tabs.update(tabId, { active: true });
      return { tabId, windowId: tab.windowId };
    },
    get: (tabId) => chrome.tabs.get(tabId),
    navigate: async (tabId, url) => {
      await chrome.tabs.update(tabId, { url, active: true });
    },
    reload: (tabId) => chrome.tabs.reload(tabId),
    ping: (tabId) =>
      bounded(
        chrome.tabs.sendMessage<BridgePong>(tabId, { kind: MSG.bridgePing }, { frameId: 0 }),
        2_000,
      ).catch(() => null),
    async instagramUsername(tabId) {
      const results = await chrome.scripting.executeScript({
        target: { tabId, frameIds: [0] },
        world: 'MAIN',
        func: currentInstagramUser,
        args: [],
      });
      return (
        (results[0]?.result as { username: string | null; login: boolean } | undefined) ?? {
          username: null,
          login: false,
        }
      );
    },
    async folderLinks(tabId) {
      const results = await chrome.scripting.executeScript({
        target: { tabId, frameIds: [0] },
        world: 'MAIN',
        func: instagramFolderLinks,
        args: [],
      });
      return Array.isArray(results[0]?.result) ? (results[0].result as string[]) : [];
    },
  };
}
