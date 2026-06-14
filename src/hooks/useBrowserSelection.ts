import { useEffect, useRef, useState } from 'react';
import { sanitizeInterceptedBatch, type SanitizedItem } from '../lib/browserSanitize';

// The three Browser platforms; all per-tab state/refs are keyed by these ids
// (mirrors BrowserTab in views/Browser.tsx).
type BrowserTab = 'instagram' | 'twitter' | 'pinterest';

// Minimal imperative surface of the Electron <webview> guest element the
// selection machinery touches. Mirrors ElectronWebview in views/Browser.tsx so
// the shared per-tab ref containers are assignable across the boundary.
interface ElectronWebview extends HTMLElement {
  getURL(): string;
  loadURL(url: string): Promise<void>;
  reload(): void;
  goBack(): void;
  goForward(): void;
  canGoBack(): boolean;
  canGoForward(): boolean;
  executeJavaScript(code: string): Promise<unknown>;
}

type WebviewRefs = Record<BrowserTab, React.MutableRefObject<ElectronWebview | null>>;
type PerTabState<T> = Record<BrowserTab, T>;

// Result of importSelected: ok=false signals a no-op (no webview, busy, nothing
// selected, collect failed) or an error, so callers can surface it.
export interface ImportSelectedResult {
  ok: boolean;
  reason?: 'no-webview' | 'busy' | 'collect-failed' | 'empty' | 'error';
  error?: unknown;
}

export interface UseBrowserSelectionOptions {
  activeTab: BrowserTab;
  currentUrl: string;
  isOnSavedPage: boolean;
  webviewRefs: WebviewRefs;
  selectScriptRef: React.MutableRefObject<string>;
  setLibraryTotal: React.Dispatch<React.SetStateAction<number | null>>;
  onSavingChange?: (saving: boolean) => void;
  onSaved?: (info: { count: number; platform: BrowserTab }) => void;
  onCollectionsChanged?: () => void;
}

export interface UseBrowserSelection {
  selectMode: PerTabState<boolean>;
  selectModeRef: React.MutableRefObject<PerTabState<boolean>>;
  selectedCount: PerTabState<number>;
  setSelectedCount: React.Dispatch<React.SetStateAction<PerTabState<number>>>;
  importingSel: PerTabState<boolean>;
  activePost: Shelfy.Post | null;
  setActivePost: React.Dispatch<React.SetStateAction<Shelfy.Post | null>>;
  toggleSelectMode: (tab?: BrowserTab) => void;
  importSelected: (tab?: BrowserTab, collectionId?: number | null) => Promise<ImportSelectedResult>;
}

const ZERO: PerTabState<number> = { instagram: 0, twitter: 0, pinterest: 0 };
const FALSE: PerTabState<boolean> = { instagram: false, twitter: false, pinterest: false };

// Manual selection mode (per-tab): draws checkboxes over each post in the
// webview so the user can cherry-pick posts and import just those.
// Owns the overlay toggle, the selected counter, the "import selected" action
// and the preview of an already-saved post opened from the "Già in database"
// label.
export default function useBrowserSelection({
  activeTab,
  currentUrl,
  isOnSavedPage,
  webviewRefs,
  selectScriptRef,
  setLibraryTotal,
  onSavingChange,
  onSaved,
  onCollectionsChanged,
}: UseBrowserSelectionOptions): UseBrowserSelection {
  const [selectMode, setSelectMode] = useState<PerTabState<boolean>>({ ...FALSE });
  const [selectedCount, setSelectedCount] = useState<PerTabState<number>>({ ...ZERO });
  // Post opened from the "Già in database" label (preview of an already-saved post).
  const [activePost, setActivePost] = useState<Shelfy.Post | null>(null);
  const [importingSel, setImportingSel] = useState<PerTabState<boolean>>({ ...FALSE });
  const selectModeRef = useRef<PerTabState<boolean>>({ ...FALSE });

  useEffect(() => {
    selectModeRef.current = selectMode;
  }, [selectMode]);

  // Toggle the in-webview selection overlay for a tab. Ensures the overlay script
  // is present (idempotent), then enable()/disable() it in the MAIN world.
  const toggleSelectMode = (tab: BrowserTab = activeTab): void => {
    const wv = webviewRefs[tab].current;
    const next = !selectMode[tab];
    setSelectMode((s) => ({ ...s, [tab]: next }));
    if (!next) setSelectedCount((c) => ({ ...c, [tab]: 0 }));
    if (!wv) return;
    const ensure = selectScriptRef.current
      ? wv.executeJavaScript(selectScriptRef.current).catch(() => {})
      : Promise.resolve();
    ensure.then(() =>
      wv
        .executeJavaScript(
          `window.__ssSelect && window.__ssSelect.${next ? 'enable' : 'disable'}()`,
        )
        .catch(() => {}),
    );
  };

  // "Importa selezionati": pull the full records for the checked posts out of the
  // overlay and import them into the library (same path the auto-import uses).
  // Media download is NOT triggered here — that's the Download screen's job. Finally
  // flag them as saved and clear.
  // Resolves to { ok, reason? }: ok=false signals a no-op (no webview, already
  // running, nothing selected, or the overlay couldn't be read) so callers can
  // surface it instead of the work silently never happening.
  const importSelected = async (
    tab: BrowserTab = activeTab,
    collectionId: number | null = null,
  ): Promise<ImportSelectedResult> => {
    const wv = webviewRefs[tab].current;
    if (!wv) return { ok: false, reason: 'no-webview' };
    if (importingSel[tab]) return { ok: false, reason: 'busy' };
    let items: SanitizedItem[] = [];
    try {
      const json = await wv.executeJavaScript(
        'window.__ssSelect ? window.__ssSelect.collectJSON() : "[]"',
      );
      const parsed: unknown = JSON.parse(typeof json === 'string' ? json || '[]' : '[]');
      // collectJSON runs in the page's MAIN world, so the payload is
      // page-controlled — sanitize it like the auto-import path does, forcing
      // each item's platform to this tab's (page-declared platforms are ignored).
      items = sanitizeInterceptedBatch(Array.isArray(parsed) ? parsed : [], tab);
    } catch {
      return { ok: false, reason: 'collect-failed' };
    }
    if (!items.length) return { ok: false, reason: 'empty' };
    setImportingSel((s) => ({ ...s, [tab]: true }));
    onSavingChange?.(true);
    try {
      const { inserted } = await window.electronAPI.saveInterceptedPosts(items, tab);
      setLibraryTotal((n) => (n === null ? inserted : n + inserted));
      // Surface the result in the Activity center ("N post salvati").
      onSaved?.({ count: inserted ?? items.length, platform: tab });
      const ids = items.map((it) => String(it.id)).filter(Boolean);
      // Folder import: file the selected posts into the chosen tag.
      if (collectionId != null && ids.length) {
        try {
          await window.electronAPI.addPostsToCollections(ids, [collectionId]);
        } catch {
          /* keep going */
        }
        onCollectionsChanged?.();
      }
      // Flag them saved via { key, id } pairs (key = shortcode on IG, id on TW).
      const savedPairs = items
        .map((it) => ({
          key:
            tab === 'twitter' || tab === 'pinterest'
              ? String(it.id)
              : String(it.shortcode || it.id),
          id: String(it.id),
        }))
        .filter((p) => p.key);
      wv.executeJavaScript(
        `window.__ssSelect && (window.__ssSelect.markSaved(${JSON.stringify(savedPairs)}), window.__ssSelect.clearSelection())`,
      ).catch(() => {});
      setSelectedCount((c) => ({ ...c, [tab]: 0 }));
      return { ok: true };
    } catch (err) {
      console.error('importSelected failed:', err);
      return { ok: false, reason: 'error', error: err };
    } finally {
      setImportingSel((s) => ({ ...s, [tab]: false }));
      onSavingChange?.(false);
    }
  };

  // Leaving the saved page tears down the overlay's context (posts are gone), so
  // exit select mode AND disable the overlay to keep the toolbar and webview in
  // sync. Opening a post is NOT leaving: it's a modal over the still-mounted grid
  // (in-page nav to /p|reel|tv/… or /status/…), so we keep the selection — the
  // overlay self-hides on the detail page and re-draws the same picks on return.
  useEffect(() => {
    const isPostDetail =
      activeTab === 'instagram'
        ? /\/(?:p|reel|tv)\/[^/]/.test(currentUrl)
        : activeTab === 'pinterest'
          ? /\/pin\/\d+/.test(currentUrl)
          : /\/status\/\d+/.test(currentUrl);
    if (!isOnSavedPage && !isPostDetail && selectMode[activeTab]) {
      setSelectMode((s) => ({ ...s, [activeTab]: false }));
      setSelectedCount((c) => ({ ...c, [activeTab]: 0 }));
      const wv = webviewRefs[activeTab]?.current;
      if (wv)
        wv.executeJavaScript('window.__ssSelect && window.__ssSelect.disable()').catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOnSavedPage, activeTab, currentUrl]);

  return {
    selectMode,
    selectModeRef,
    selectedCount,
    setSelectedCount,
    importingSel,
    activePost,
    setActivePost,
    toggleSelectMode,
    importSelected,
  };
}
