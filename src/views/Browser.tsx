import React, { useRef, useState, useEffect } from 'react';
import {
  RefreshCw,
  ChevronLeft,
  ChevronRight,
  CheckSquare,
  Import,
  X,
  Loader2,
  type LucideIcon,
} from 'lucide-react';
import ImportFolderModal from '../components/ImportFolderModal';
import PostModal from '../components/PostModal';
import { useT } from '../i18n';
import {
  TABS,
  SAVED_PATTERNS,
  isPostDetail,
  parseIgFolder,
  parsePinBoard,
  deslugify,
  safeUrl,
} from '../lib/browserUrls';
import { readIgFolderName } from '../lib/browserScripts';
import useBrowserScripts from '../hooks/useBrowserScripts';
import useBrowserSync from '../hooks/useBrowserSync';
import useSourceSync from '../hooks/useSourceSync';
import useBrowserWebview from '../hooks/useBrowserWebview';
import useBrowserSelection from '../hooks/useBrowserSelection';
import useBrowserIntercept from '../hooks/useBrowserIntercept';

// The three platforms the in-app Browser drives (a subset of Shelfy.Platform).
// All per-tab state in the browser hooks is keyed by these ids.
type BrowserTab = 'instagram' | 'twitter' | 'pinterest';

// The Electron <webview> guest element. The renderer tsconfig only pulls in
// vite/client types (not the global Electron namespace), so the imperative
// surface the Browser + its hooks touch is declared minimally here. The shared
// per-tab ref containers hold one of these per platform.
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

// <webview> already exists as a JSX intrinsic in @types/react (typed against the
// empty global HTMLWebViewElement). Augment that element with the imperative
// surface the Browser + its hooks call so refs to the guest element are typed.
declare global {
  interface HTMLWebViewElement extends ElectronWebview {}
}

function formatElapsed(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

interface AnimatedNumberProps {
  value: number;
  duration?: number;
  style?: React.CSSProperties;
}

// Smoothly ticks the displayed number from its previous value to the target.
// Batches arrive in chunks (e.g. 20 tweets per Bookmarks response), so this
// animates the jump to give a real-time feel rather than a sudden leap.
function AnimatedNumber({ value, duration = 500, style }: AnimatedNumberProps): React.JSX.Element {
  const [display, setDisplay] = useState<number>(value);
  // Tracks the value currently on screen, updated every frame: when `value`
  // changes mid-animation the next run starts from here instead of jumping back
  // to the previous animation's stale starting point.
  const displayRef = useRef<number>(value);
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    const from = displayRef.current;
    const to = value;
    if (from === to) return;
    const start = performance.now();
    const step = (now: number): void => {
      const t = Math.min((now - start) / duration, 1);
      const eased = 1 - Math.pow(1 - t, 3); // easeOutCubic
      const next = Math.round(from + (to - from) * eased);
      displayRef.current = next;
      setDisplay(next);
      if (t < 1) rafRef.current = requestAnimationFrame(step);
    };
    rafRef.current = requestAnimationFrame(step);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
    };
  }, [value, duration]);

  return <span style={style}>{display}</span>;
}

// The folder-import modal state, opened when syncing/importing from inside a
// native IG folder or Pinterest board. `action` decides what runs once confirmed.
interface FolderModalState {
  platform: BrowserTab;
  externalId: string;
  // Proposed tag name: the existing (possibly renamed) tag, else the source's name.
  suggestedName: string;
  igName: string;
  matchedId: number | null;
  action: 'sync' | 'selected';
}

// The choice ImportFolderModal resolves to on confirm: either file into a tag
// (existing id, or a new tag to create) or skip tagging entirely.
interface FolderChoice {
  mode: 'tag' | 'platform';
  collectionId?: number | null;
  name?: string;
  color?: string;
}

interface BrowserProps {
  activeTab?: BrowserTab;
  onSyncingChange?: (syncing: Record<BrowserTab, boolean>) => void;
  onSavingChange?: (saving: boolean) => void;
  onSaved?: (info: { count: number; platform: BrowserTab }) => void;
  collections?: Shelfy.Collection[];
  onCreateCollection?: (
    name: string,
    color: string,
    opts: { platform: BrowserTab; externalId: string; igName: string },
  ) => Promise<Shelfy.Collection | null | undefined>;
  onCollectionsChanged?: () => void;
  registerSourceSyncApi?: (api: unknown) => void;
  onSourceSyncJobs?: (jobs: unknown) => void;
}

export default function Browser({
  activeTab = 'instagram',
  onSyncingChange,
  onSavingChange,
  onSaved,
  collections = [],
  onCreateCollection,
  onCollectionsChanged,
  registerSourceSyncApi,
  onSourceSyncJobs,
}: BrowserProps): React.JSX.Element {
  const t = useT('browser');
  // Stable per-tab ref containers created once. Using useRef(...).current keeps a
  // fixed number of hook calls (Rules of Hooks) instead of calling useRef inside
  // an object literal that React re-evaluates every render. Shared with every
  // browser hook (sync, navigation, selection, intercept).
  const webviewRefs = useRef<Record<BrowserTab, React.MutableRefObject<ElectronWebview | null>>>({
    instagram: { current: null },
    twitter: { current: null },
    pinterest: { current: null },
  }).current;

  // MAIN-world scripts (capture hook + selection overlay) + inject-once guard.
  const {
    injectedScriptRef,
    selectScriptRef,
    injectedForLoadRefs,
    scriptsStatus,
    retryLoadScripts,
  } = useBrowserScripts();

  // Sync lifecycle: per-tab flags/counters/timers, pre-sync buffer, start/stop.
  const {
    syncing,
    syncingRef,
    interceptedCount,
    syncScanned,
    syncNew,
    syncSkipped,
    syncElapsed,
    libraryTotal,
    setLibraryTotal,
    syncCollectionRefs,
    syncScriptPromiseRefs,
    syncCountsRefs,
    pendingRefs,
    finishSync,
    ingestBatch,
    startSync,
    stopSync,
  } = useBrowserSync({
    activeTab,
    webviewRefs,
    injectedScriptRef,
    onSyncingChange,
    onCollectionsChanged,
  });

  // Background source-sync launched from the Library sidebar: navigates the
  // hidden webviews to the right listing and drives startSync there. Exposes an
  // imperative start/stop/dismiss API up to App and reports its jobs for the
  // Activity Center; runRef gates the saved-URL auto-restore below.
  const {
    runRef: sourceSyncRunRef,
    jobs: sourceSyncJobs,
    stop: stopSourceSync,
  } = useSourceSync({
    webviewRefs,
    syncingRef,
    startSync,
    stopSync,
    syncScriptPromiseRefs,
    syncCountsRefs,
    scriptsStatus,
    injectedForLoadRefs,
    syncScanned,
    syncNew,
    collections,
    registerApi: registerSourceSyncApi,
    onJobsChange: onSourceSyncJobs,
  });

  // Webview navigation: urls/loading/canGo* state, nav listeners, back/forward/refresh.
  const {
    urls,
    loading,
    canGoBack,
    canGoForward,
    igInitialUrl,
    pinInitialUrl,
    handleRefresh,
    handleBack,
    handleForward,
  } = useBrowserWebview({
    activeTab,
    webviewRefs,
    syncingRef,
    pendingRefs,
    injectedForLoadRefs,
    stopSync,
    externalNavRef: sourceSyncRunRef,
  });

  const currentUrl = urls[activeTab];
  const isOnSavedPage = SAVED_PATTERNS[activeTab]?.test(currentUrl);
  // Opening a post/pin detail is an in-page nav with the listing still mounted
  // behind the modal — same exception useBrowserWebview makes — so it must not
  // trip the active-tab auto-stop below.
  const isOnPostDetail = isPostDetail(currentUrl);
  // Non-null when the Instagram tab is INSIDE a saved folder (not all-posts).
  const igFolder = activeTab === 'instagram' ? parseIgFolder(currentUrl) : null;
  // Pinterest equivalent of an IG saved-folder: the board currently being viewed.
  const pinBoard = activeTab === 'pinterest' ? parsePinBoard(currentUrl) : null;
  const isSyncing = syncing[activeTab];
  const isFetching = isSyncing && loading[activeTab];
  // A background source-sync run owns this tab (it navigates it and drives
  // startSync): the Auto-import button must not start a competing manual sync —
  // it becomes a stop for the whole run instead.
  const sourceSyncJob = sourceSyncJobs[activeTab];
  const sourceSyncActive =
    !!sourceSyncJob &&
    (sourceSyncJob.status === 'navigating' || sourceSyncJob.status === 'syncing');
  const importStops = isSyncing || sourceSyncActive;
  // The MAIN-world scripts must be loaded before a sync/selection can capture
  // anything; until then (or if they failed to load) the capture/select controls
  // are disabled. A running sync can always be stopped regardless.
  const scriptsReady = scriptsStatus === 'ready';
  const captureDisabled = !scriptsReady && !importStops;

  // Selection mode: overlay toggle, selected counter, "import selected" action.
  const {
    selectMode,
    selectModeRef,
    selectedCount,
    setSelectedCount,
    importingSel,
    activePost,
    setActivePost,
    toggleSelectMode,
    importSelected,
  } = useBrowserSelection({
    activeTab,
    currentUrl,
    isOnSavedPage,
    webviewRefs,
    selectScriptRef,
    setLibraryTotal,
    onSavingChange,
    onSaved,
    onCollectionsChanged,
  });

  // Webview → host messages (intercepted batches + selection overlay) and
  // MAIN-world script injection. Attached once on mount.
  useBrowserIntercept({
    webviewRefs,
    injectedScriptRef,
    selectScriptRef,
    selectModeRef,
    injectedForLoadRefs,
    syncingRef,
    pendingRefs,
    syncCollectionRefs,
    ingestBatch,
    finishSync,
    setSelectedCount,
    setActivePost,
  });

  useEffect(() => {
    if (!isOnSavedPage && !isOnPostDetail && isSyncing) stopSync(activeTab);
    // stopSync intentionally omitted from deps: its identity changes every render
    // but it reads only refs and stable setters so there's no stale-closure risk.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOnSavedPage, isOnPostDetail, isSyncing]);

  // Folder-import modal: { folderId, suggestedName, igName, matchedId, action }.
  const [folderModal, setFolderModal] = useState<FolderModalState | null>(null);

  // Opens the folder-import modal, resolving a sensible default tag name (the
  // folder's real title, falling back to the de-slugified URL) and the tag already
  // linked to this folder, if any. `action` decides what runs once confirmed.
  const openFolderModal = async (action: FolderModalState['action']): Promise<void> => {
    const folder = igFolder || pinBoard;
    if (!folder) return;
    const platform = activeTab; // 'instagram' | 'pinterest'
    const externalId = igFolder ? igFolder.folderId : pinBoard!.boardId;
    const wv = webviewRefs[platform].current;
    const fallback = deslugify(folder.slug, t('folderFallback'));
    const name = wv ? await readIgFolderName(wv, fallback) : fallback;
    const matched = (collections || []).find(
      (c) => c.platform === platform && String(c.externalId) === String(externalId),
    );
    setFolderModal({
      platform,
      externalId,
      // Propose the existing (possibly renamed) tag name; else the source's name.
      suggestedName: matched?.name || name,
      igName: name,
      matchedId: matched?.id ?? null,
      action,
    });
  };

  // Resolve the modal choice into a collection id (creating a tag if needed), then
  // run the pending action (full sync or selection import) toward that destination.
  const handleFolderConfirm = async (choice: FolderChoice): Promise<void> => {
    if (!folderModal) return;
    const { action, platform, externalId, igName } = folderModal;
    let collectionId: number | null = null;
    if (choice.mode === 'tag') {
      if (choice.collectionId != null) {
        collectionId = choice.collectionId;
      } else {
        const created = await onCreateCollection?.(choice.name ?? '', choice.color ?? '', {
          platform,
          externalId,
          igName,
        });
        collectionId = created?.id ?? null;
        onCollectionsChanged?.();
      }
    }
    // Await the action and propagate a no-op/failure as a rejection so the modal
    // (which awaits onConfirm before closing) can keep itself open / show an error
    // instead of vanishing while the import silently never happened.
    if (action === 'selected') {
      const res = await importSelected(platform, collectionId);
      if (!res || !res.ok) {
        if (res?.error) throw res.error;
        throw new Error(res?.reason === 'empty' ? t('errNoSelection') : t('errStartImport'));
      }
    } else {
      // A source-sync run may have grabbed this tab while the modal was open:
      // starting a manual sync now would race it (shared counters/collection
      // target, competing navigation). Refuse instead.
      if (sourceSyncRunRef.current[platform]) throw new Error(t('errSourceSyncActive'));
      const started = startSync(platform, collectionId);
      if (!started) throw new Error(t('errStartSync'));
    }
  };

  // Import button: stop the source-sync run if one owns this tab, stop the
  // manual sync if running; otherwise gate behind the folder modal when inside
  // an Instagram folder, else start a plain full sync.
  const handleImportClick = (): void => {
    // jobs state (sourceSyncActive) lags runRef by one render: check both so a
    // just-started run can't race a manual sync in that window.
    if (sourceSyncActive || sourceSyncRunRef.current[activeTab]) {
      stopSourceSync(activeTab);
      return;
    }
    if (isSyncing) {
      stopSync();
      return;
    }
    if (igFolder || pinBoard) {
      openFolderModal('sync');
      return;
    }
    startSync();
  };

  // "Importa selezionati": same folder gating as the full import.
  const handleImportSelectedClick = (): void => {
    if (igFolder || pinBoard) {
      openFolderModal('selected');
      return;
    }
    importSelected();
  };

  return (
    <div
      data-testid="browser-view"
      style={{ display: 'flex', flexDirection: 'column', height: '100%', background: '#0f0f0f' }}
    >
      {/* Top bar */}
      <div
        style={{
          height: 52,
          minHeight: 52,
          display: 'flex',
          alignItems: 'center',
          gap: 12,
          padding: '0 16px',
          background: '#0f0f0f',
          borderBottom: '1px solid #2e2e2e',
          flexShrink: 0,
        }}
      >
        {/* Back / Forward */}
        {(
          [
            {
              id: 'back',
              label: t('back'),
              icon: ChevronLeft,
              action: handleBack,
              enabled: canGoBack[activeTab],
              testId: 'browser-back',
            },
            {
              id: 'forward',
              label: t('forward'),
              icon: ChevronRight,
              action: handleForward,
              enabled: canGoForward[activeTab],
              testId: 'browser-forward',
            },
          ] as Array<{
            id: string;
            label: string;
            icon: LucideIcon;
            action: () => void;
            enabled: boolean;
            testId: string;
          }>
        ).map(({ id, label, icon: Icon, action, enabled, testId }) => (
          <button
            key={id}
            className="u-press"
            data-testid={testId}
            onClick={action}
            disabled={!enabled}
            title={label}
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              width: 30,
              height: 30,
              borderRadius: 6,
              border: '1px solid #2e2e2e',
              background: '#1a1a1a',
              color: enabled ? '#9ca3af' : '#383838',
              cursor: enabled ? 'pointer' : 'default',
              flexShrink: 0,
            }}
            onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
              if (enabled) {
                e.currentTarget.style.background = '#2a2a2a';
                e.currentTarget.style.color = '#f0f0f0';
              }
            }}
            onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
              e.currentTarget.style.background = '#1a1a1a';
              e.currentTarget.style.color = enabled ? '#9ca3af' : '#383838';
            }}
          >
            <Icon size={15} />
          </button>
        ))}

        {/* URL bar */}
        <div
          data-testid="url-bar"
          style={{
            flex: 1,
            background: '#1a1a1a',
            border: '1px solid #2e2e2e',
            borderRadius: 6,
            padding: '6px 10px',
            fontSize: 12,
            color: '#9ca3af',
            overflow: 'hidden',
            whiteSpace: 'nowrap',
            textOverflow: 'ellipsis',
            userSelect: 'text',
            cursor: 'default',
            fontFamily: 'ui-monospace, monospace',
          }}
          title={currentUrl}
        >
          {currentUrl}
        </div>

        {/* Refresh button */}
        <button
          className="u-press"
          data-testid="browser-refresh"
          onClick={handleRefresh}
          title={t('reload')}
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            width: 30,
            height: 30,
            borderRadius: 6,
            border: '1px solid #2e2e2e',
            background: '#1a1a1a',
            color: '#9ca3af',
            cursor: 'pointer',
            flexShrink: 0,
          }}
          onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.background = '#2a2a2a';
            e.currentTarget.style.color = '#f0f0f0';
          }}
          onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.style.background = '#1a1a1a';
            e.currentTarget.style.color = '#9ca3af';
          }}
        >
          <RefreshCw size={14} className={loading[activeTab] ? 'u-spin' : undefined} />
        </button>

        {/* Divider */}
        <div style={{ width: 1, height: 20, background: '#2e2e2e', flexShrink: 0 }} />

        {/* Intercepting status + Sync button */}
        <div
          data-testid="browser-status"
          style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0, fontSize: 12 }}
        >
          {isOnSavedPage ? (
            <>
              {!isSyncing && (
                <span
                  className="u-fade-in"
                  style={{
                    color: '#4caf50',
                    display: 'flex',
                    alignItems: 'center',
                    gap: 6,
                    whiteSpace: 'nowrap',
                    flexShrink: 0,
                  }}
                >
                  {/* Round dot — u-glow's box-shadow halo follows border-radius, so the
                      element must be an actual circle (an inline ● glows as a square). */}
                  <span
                    className="u-glow"
                    style={{
                      width: 7,
                      height: 7,
                      borderRadius: '50%',
                      background: '#4caf50',
                      flexShrink: 0,
                    }}
                  />
                  {t('intercepting')}
                </span>
              )}
              <button
                className="u-press"
                onClick={handleImportClick}
                disabled={captureDisabled}
                title={
                  sourceSyncActive
                    ? t('stopSourceSyncTitle')
                    : isSyncing
                      ? t('stopSyncTitle')
                      : captureDisabled
                        ? scriptsStatus === 'error'
                          ? t('scriptsErrorCapture')
                          : t('scriptsLoadingCapture')
                        : igFolder || pinBoard
                          ? t('autoImportFolderTitle')
                          : t('autoImportAllTitle')
                }
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 6,
                  padding: '3px 10px',
                  borderRadius: 6,
                  border: 'none',
                  cursor: captureDisabled ? 'default' : 'pointer',
                  fontSize: 12,
                  fontWeight: 500,
                  background: importStops ? '#3a1416' : '#1f1f1f',
                  color: importStops ? '#ff6b6b' : '#d0d0d0',
                  outline: 'none',
                  opacity: captureDisabled ? 0.45 : 1,
                  flexShrink: 0,
                }}
                onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                  if (!captureDisabled)
                    e.currentTarget.style.background = importStops ? '#4a191c' : '#2a2a2a';
                }}
                onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                  e.currentTarget.style.background = importStops ? '#3a1416' : '#1f1f1f';
                }}
              >
                {/* REC-style indicator: steady red dot when idle, pulsing while syncing */}
                <span
                  style={{
                    width: 9,
                    height: 9,
                    borderRadius: '50%',
                    background: '#ff3b30',
                    flexShrink: 0,
                    animation: importStops ? 'rec-pulse 1.2s ease-in-out infinite' : 'none',
                  }}
                />
                {importStops ? t('stop') : t('autoImport')}
              </button>

              {/* Selection mode — checkboxes over each post + "scarica selezionati" */}
              {!selectMode[activeTab] ? (
                <button
                  className="u-press u-fade-in"
                  data-testid="select-toggle"
                  onClick={() => toggleSelectMode()}
                  disabled={!scriptsReady}
                  title={
                    scriptsReady
                      ? t('selectTitle')
                      : scriptsStatus === 'error'
                        ? t('scriptsErrorSelect')
                        : t('scriptsLoadingSelect')
                  }
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 6,
                    padding: '3px 10px',
                    borderRadius: 6,
                    border: '1px solid #2e2e2e',
                    background: '#1a1a1a',
                    color: '#9ca3af',
                    cursor: scriptsReady ? 'pointer' : 'default',
                    fontSize: 12,
                    fontWeight: 500,
                    opacity: scriptsReady ? 1 : 0.45,
                    flexShrink: 0,
                  }}
                  onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                    if (!scriptsReady) return;
                    e.currentTarget.style.background = '#2a2a2a';
                    e.currentTarget.style.color = '#f0f0f0';
                  }}
                  onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                    e.currentTarget.style.background = '#1a1a1a';
                    e.currentTarget.style.color = '#9ca3af';
                  }}
                >
                  <CheckSquare size={14} />
                  {t('select')}
                </button>
              ) : (
                <div
                  className="u-scale-in"
                  style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}
                >
                  <span
                    data-testid="selection-count"
                    style={{
                      fontSize: 12,
                      color: '#9ca3af',
                      fontVariantNumeric: 'tabular-nums',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    <span
                      key={selectedCount[activeTab]}
                      className="u-pop-in"
                      style={{ display: 'inline-block' }}
                    >
                      {selectedCount[activeTab]}
                    </span>{' '}
                    {t('selectedCount')}
                  </span>
                  <button
                    className="u-press u-transition"
                    data-testid="import-selected"
                    onClick={handleImportSelectedClick}
                    disabled={selectedCount[activeTab] === 0 || importingSel[activeTab]}
                    title={t('importSelectedTitle')}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      gap: 6,
                      padding: '3px 12px',
                      borderRadius: 6,
                      border: 'none',
                      background: '#7B5CFF',
                      color: '#fff',
                      cursor:
                        selectedCount[activeTab] === 0 || importingSel[activeTab]
                          ? 'default'
                          : 'pointer',
                      fontSize: 12,
                      fontWeight: 600,
                      opacity: selectedCount[activeTab] === 0 || importingSel[activeTab] ? 0.4 : 1,
                      flexShrink: 0,
                    }}
                    onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                      if (selectedCount[activeTab] > 0 && !importingSel[activeTab])
                        e.currentTarget.style.background = '#5A3DDE';
                    }}
                    onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                      e.currentTarget.style.background = '#7B5CFF';
                    }}
                  >
                    {importingSel[activeTab] ? (
                      <Loader2 size={14} className="animate-spin" />
                    ) : (
                      <Import size={14} />
                    )}
                    {t('importSelected')}
                  </button>
                  <button
                    className="u-press"
                    data-testid="select-exit"
                    onClick={() => toggleSelectMode()}
                    title={t('exitSelectionTitle')}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      justifyContent: 'center',
                      width: 26,
                      height: 26,
                      borderRadius: 6,
                      border: '1px solid #2e2e2e',
                      background: '#1a1a1a',
                      color: '#9ca3af',
                      cursor: 'pointer',
                      flexShrink: 0,
                    }}
                    onMouseEnter={(e: React.MouseEvent<HTMLButtonElement>) => {
                      e.currentTarget.style.background = '#2a2a2a';
                      e.currentTarget.style.color = '#f0f0f0';
                    }}
                    onMouseLeave={(e: React.MouseEvent<HTMLButtonElement>) => {
                      e.currentTarget.style.background = '#1a1a1a';
                      e.currentTarget.style.color = '#9ca3af';
                    }}
                  >
                    <X size={15} />
                  </button>
                </div>
              )}

              {/* Capture-script load failure: the hook/overlay never installed, so
                  a sync would silently capture nothing. Surface it with a retry. */}
              {scriptsStatus === 'error' && (
                <button
                  className="u-press u-fade-in"
                  data-testid="scripts-retry"
                  onClick={retryLoadScripts}
                  title={t('reloadCaptureScriptsTitle')}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 6,
                    padding: '3px 10px',
                    borderRadius: 6,
                    border: '1px solid #5a1f22',
                    background: '#3a1416',
                    color: '#ff8a80',
                    cursor: 'pointer',
                    fontSize: 12,
                    fontWeight: 500,
                    flexShrink: 0,
                  }}
                >
                  <RefreshCw size={13} />
                  {t('captureUnavailable')}
                </button>
              )}
            </>
          ) : (
            <span
              className="u-fade-in"
              style={{ color: '#606060', display: 'flex', alignItems: 'center', gap: 4 }}
            >
              <span style={{ fontSize: 10 }}>○</span>
              {t('navigateToSaved')}
            </span>
          )}
        </div>

        {/* Captured count (manual scroll, non-sync) */}
        {!isSyncing && interceptedCount[activeTab] > 0 && (
          <>
            <div style={{ width: 1, height: 20, background: '#2e2e2e', flexShrink: 0 }} />
            <span
              className="u-fade-in-down"
              style={{
                fontSize: 12,
                color: '#7B5CFF',
                flexShrink: 0,
                fontVariantNumeric: 'tabular-nums',
              }}
            >
              {t('captured', { count: interceptedCount[activeTab] })}
            </span>
          </>
        )}
      </div>

      {/* Sync status bar */}
      {isSyncing && (
        <div
          className="u-fade-in-down"
          style={{
            position: 'relative',
            height: 36,
            minHeight: 36,
            display: 'flex',
            alignItems: 'center',
            padding: '0 16px',
            background: '#131300',
            borderBottom: '1px solid #2e2200',
            flexShrink: 0,
            overflow: 'hidden',
            gap: 10,
            fontSize: 12,
          }}
        >
          {/* Progress bar */}
          <div
            style={{
              position: 'absolute',
              top: 0,
              left: 0,
              right: 0,
              height: 2,
              background: '#1e1800',
              overflow: 'hidden',
            }}
          >
            <div
              style={{
                height: '100%',
                background: isFetching ? '#ff9800' : '#5a4500',
                animation: isFetching
                  ? 'sync-fetch 0.9s ease-in-out infinite'
                  : 'sync-scroll 2s ease-in-out infinite',
              }}
            />
          </div>

          {/* Pulsing dot */}
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: '50%',
              background: isFetching ? '#ff9800' : '#c8800a',
              flexShrink: 0,
              animation: isFetching
                ? 'sync-pulse-fast 0.6s ease-in-out infinite'
                : 'sync-pulse-slow 1.8s ease-in-out infinite',
            }}
          />

          {/* Status text */}
          <span style={{ color: isFetching ? '#ffb74d' : '#a07030', whiteSpace: 'nowrap' }}>
            {isFetching ? t('fetchingNew') : t('scrollingMore')}
          </span>

          <div style={{ flex: 1 }} />

          {/* Stats */}
          <span
            style={{
              color: '#806030',
              fontVariantNumeric: 'tabular-nums',
              whiteSpace: 'nowrap',
              fontSize: 12,
            }}
          >
            {t('scanned')}{' '}
            <AnimatedNumber
              key={`scanned-${activeTab}`}
              value={syncScanned[activeTab]}
              style={{ color: syncScanned[activeTab] > 0 ? '#ffb74d' : '#555', fontWeight: 600 }}
            />
          </span>
          <span style={{ color: '#3a2a00' }}>·</span>
          <span
            style={{
              color: '#806030',
              fontVariantNumeric: 'tabular-nums',
              whiteSpace: 'nowrap',
              fontSize: 12,
            }}
          >
            {t('new')}{' '}
            <AnimatedNumber
              key={`new-${activeTab}`}
              value={syncNew[activeTab]}
              style={{ color: syncNew[activeTab] > 0 ? '#ffcc80' : '#555', fontWeight: 600 }}
            />
          </span>
          {syncSkipped[activeTab] > 0 && (
            <>
              <span style={{ color: '#3a2a00' }}>·</span>
              <span
                className="u-fade-in"
                style={{
                  color: '#3d3520',
                  fontVariantNumeric: 'tabular-nums',
                  whiteSpace: 'nowrap',
                  fontSize: 11,
                }}
              >
                {t('existing', { count: syncSkipped[activeTab] })}
              </span>
            </>
          )}

          {/* Divider */}
          <div style={{ width: 1, height: 14, background: '#3a2a00', flexShrink: 0 }} />

          {/* Library total */}
          {libraryTotal !== null && (
            <span
              style={{
                color: '#4a3820',
                fontVariantNumeric: 'tabular-nums',
                whiteSpace: 'nowrap',
                fontSize: 11,
              }}
            >
              {t('library')}{' '}
              <AnimatedNumber value={libraryTotal} style={{ color: '#806030', fontWeight: 600 }} />
            </span>
          )}

          {/* Divider */}
          <div style={{ width: 1, height: 14, background: '#3a2a00', flexShrink: 0 }} />

          {/* Timer */}
          <span
            style={{
              color: '#604820',
              fontVariantNumeric: 'tabular-nums',
              fontFamily: 'ui-monospace, monospace',
              fontSize: 11,
              whiteSpace: 'nowrap',
            }}
          >
            {formatElapsed(syncElapsed[activeTab])}
          </span>
        </div>
      )}

      {/* Webview area */}
      <div style={{ flex: 1, position: 'relative', overflow: 'hidden', background: '#fff' }}>
        {/* Loading bar (non-sync navigation) */}
        {!isSyncing && loading[activeTab] && (
          <div
            style={{
              position: 'absolute',
              top: 0,
              left: 0,
              right: 0,
              height: 2,
              zIndex: 10,
              background: '#2e2e2e',
              overflow: 'hidden',
            }}
          >
            <div
              style={{
                height: '100%',
                background: '#7B5CFF',
                animation: 'browser-progress 1.4s ease-in-out infinite',
              }}
            />
          </div>
        )}

        {/* allowpopups is required for the guest's window.open to work at all
            (OAuth "Continue with Google/Apple" popups); the main process gates
            which popups open (setWindowOpenHandler) and confines them. */}
        {TABS.map((tab) => (
          <webview
            key={tab.id}
            ref={webviewRefs[tab.id as BrowserTab]}
            src={safeUrl(
              tab.id,
              tab.id === 'instagram'
                ? igInitialUrl.current
                : tab.id === 'pinterest'
                  ? pinInitialUrl.current
                  : tab.url,
              tab.url,
            )}
            partition="persist:social"
            preload={window.electronAPI.webviewPreloadPath}
            // @types/react types allowpopups as boolean, but Electron's <webview>
            // takes it as a string attribute; the literal value is unchanged.
            allowpopups={'true' as unknown as boolean}
            webpreferences="contextIsolation=true, nodeIntegration=false, sandbox=false, backgroundThrottling=false"
            style={{
              // Stack the webviews instead of display:none-ing the inactive ones — a
              // display:none webview has no layout, so its scroll scripts can't load
              // more content. We can't rely on the active page being opaque to cover
              // the others (Pinterest's masonry grid is transparent between pins, so a
              // webview behind it bleeds through), so the inactive ones are kept laid
              // out (background sync keeps running) but painted invisible via opacity:0
              // rather than left visible underneath.
              position: 'absolute',
              inset: 0,
              width: '100%',
              height: '100%',
              // Opaque white canvas like a real browser tab: sites that don't paint
              // their own background (e.g. Pinterest's pin grid) would otherwise be
              // transparent and reveal whatever is behind the webview.
              background: '#fff',
              zIndex: activeTab === tab.id ? 2 : 1,
              opacity: activeTab === tab.id ? 1 : 0,
              pointerEvents: activeTab === tab.id ? 'auto' : 'none',
            }}
          />
        ))}
      </div>

      {folderModal && (
        <ImportFolderModal
          folderName={folderModal.suggestedName}
          igCollections={(collections || []).filter(
            (c) => c.platform === (folderModal.platform || 'instagram'),
          )}
          matchedId={folderModal.matchedId}
          platform={folderModal.platform || 'instagram'}
          actionLabel={folderModal.action === 'selected' ? t('actionDownload') : t('actionImport')}
          onClose={() => setFolderModal(null)}
          onConfirm={handleFolderConfirm}
        />
      )}

      {/* Preview of an already-saved post, opened from the "Già in database" label. */}
      {activePost && (
        <PostModal
          post={activePost}
          onClose={() => setActivePost(null)}
          onLocalFilesDeleted={() => setActivePost(null)}
        />
      )}

      <style>{`
        @keyframes browser-progress {
          0%   { transform: translateX(-100%); }
          50%  { transform: translateX(0%); }
          100% { transform: translateX(100%); }
        }
        @keyframes sync-fetch {
          0%   { transform: translateX(-100%); width: 60%; }
          50%  { transform: translateX(80%); width: 60%; }
          100% { transform: translateX(200%); width: 60%; }
        }
        @keyframes sync-scroll {
          0%   { transform: translateX(-100%); width: 30%; opacity: 0.5; }
          50%  { transform: translateX(150%); width: 30%; opacity: 0.5; }
          100% { transform: translateX(400%); width: 30%; opacity: 0.5; }
        }
        @keyframes sync-pulse-fast {
          0%, 100% { opacity: 1; transform: scale(1); }
          50%       { opacity: 0.4; transform: scale(0.7); }
        }
        @keyframes sync-pulse-slow {
          0%, 100% { opacity: 0.7; }
          50%       { opacity: 0.2; }
        }
        @keyframes rec-pulse {
          0%, 100% { opacity: 1; box-shadow: 0 0 0 0 rgba(255, 59, 48, 0.55); }
          50%       { opacity: 0.55; box-shadow: 0 0 0 4px rgba(255, 59, 48, 0); }
        }
      `}</style>
    </div>
  );
}
