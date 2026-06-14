import { useEffect, useRef, useState } from 'react';
import { sanitizeInterceptedBatch } from '../lib/browserSanitize';

const ZERO = { instagram: 0, twitter: 0, pinterest: 0 };
const FALSE = { instagram: false, twitter: false, pinterest: false };

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
}) {
  const [selectMode, setSelectMode] = useState({ ...FALSE });
  const [selectedCount, setSelectedCount] = useState({ ...ZERO });
  // Post opened from the "Già in database" label (preview of an already-saved post).
  const [activePost, setActivePost] = useState(null);
  const [importingSel, setImportingSel] = useState({ ...FALSE });
  const selectModeRef = useRef({ ...FALSE });

  useEffect(() => {
    selectModeRef.current = selectMode;
  }, [selectMode]);

  // Toggle the in-webview selection overlay for a tab. Ensures the overlay script
  // is present (idempotent), then enable()/disable() it in the MAIN world.
  const toggleSelectMode = (tab = activeTab) => {
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
  const importSelected = async (tab = activeTab, collectionId = null) => {
    const wv = webviewRefs[tab].current;
    if (!wv) return { ok: false, reason: 'no-webview' };
    if (importingSel[tab]) return { ok: false, reason: 'busy' };
    let items = [];
    try {
      const json = await wv.executeJavaScript(
        'window.__ssSelect ? window.__ssSelect.collectJSON() : "[]"',
      );
      const parsed = JSON.parse(json || '[]');
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
