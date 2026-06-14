import {
  useState,
  useEffect,
  useRef,
  useCallback,
  useMemo,
  createContext,
  useContext,
  createElement,
} from 'react';
import type { ReactElement, ReactNode } from 'react';
import { analysisSummary } from './useAnalysis';
import { translate, useLang } from '../i18n';

// ── Centro Attività ───────────────────────────────────────────────────────────
// Aggrega TUTTO ciò che l'app fa in background — auto-tag, download media, sync
// raccolte social, salvataggio post selezionati, download del modello VLM/STT,
// provisioning dei binari e aggiornamenti OTA — in un'unica lista normalizzata.
//
// Tre livelli, per riusare le sottoscrizioni IPC già esistenti (niente listener
// doppi → niente stato divergente):
//   1. buildActivities()  — selettore PURO: sorgenti → { live, summary }.
//   2. ActivityProvider    — riceve da App lo stato già sollevato (analysis,
//                            downloads, sync, save, model) e sottoscrive da sé le
//                            sorgenti senza consumer (updater, binaries, stt);
//                            tiene lo storico di sessione (log).
//   3. useActivity()       — espone { live, summary, log, unread, actions, … }.

// ── shared shapes ──────────────────────────────────────────────────────────────

// Binder for the 'activity' namespace, mirroring useT's signature.
type Translate = (key: string, vars?: Record<string, unknown>) => string;

type ActivityKind =
  | 'update'
  | 'model'
  | 'analysis'
  | 'download'
  | 'web'
  | 'save'
  | 'sync'
  | 'binaries'
  | 'stt';

// Action button rendered on a live item / accompanying a queue command.
export interface ActivityAction {
  id: string;
  label: string;
  variant: 'accent' | 'ghost' | 'danger';
}

// A normalized "in progress" row produced by buildActivities.
export interface ActivityItem {
  id: string;
  kind: ActivityKind;
  status: string; // 'running' | 'paused' | 'error' | 'available' | 'ready' | 'installing'
  title: string;
  subtitle?: string | null;
  progress?: number | null;
  count?: number;
  total?: number;
  etaMs?: number | null;
  actions?: ActivityAction[];
  platform?: string;
  postId?: string | null;
  version?: string | null;
  short?: string;
}

// Session-history entry.
export interface LogEntry {
  id: string;
  kind: ActivityKind | string;
  title: string;
  subtitle?: string | null;
  status: string;
  ts: number;
  read: boolean;
  platform?: string;
}

// Aggregate strip summary.
export interface ActivitySummary {
  activeCount: number;
  headline: string;
  hasError: boolean;
  needsAction: boolean;
  progress: number | null;
  primaryKind: ActivityKind | null;
  primaryStatus: string | null;
}

export interface BuildActivitiesResult {
  live: ActivityItem[];
  summary: ActivitySummary;
}

// ── source slice shapes (lifted state passed by App + own subscriptions) ────────

// Reuse the exact shapes the (parallel-converted) analysisSummary works with, so
// this hook stays in lockstep with useAnalysis regardless of how it's typed.
type AnalysisJobs = Parameters<typeof analysisSummary>[0];
type AnalysisSummaryShape = ReturnType<typeof analysisSummary>;

interface AnalysisSource {
  jobs?: AnalysisJobs;
  concurrency?: number;
  paused?: boolean;
  summary?: AnalysisSummaryShape;
}

interface DownloadJobLike {
  status?: string;
  progress?: number;
  authorUsername?: string | null;
}
interface DownloadsSource {
  jobs?: DownloadJobLike[];
  isPaused?: boolean;
}

interface ModelStatusShape {
  downloading?: boolean;
  ready?: boolean;
}
interface ModelProgressShape {
  progress?: number;
  label?: string | null;
}
interface ModelSource {
  status?: ModelStatusShape | null;
  progress?: ModelProgressShape | null;
}

interface SyncJobLike {
  status?: string;
  currentLabel?: string | null;
  stepIndex?: number;
  stepCount?: number;
  scanned?: number;
  fresh?: number;
  error?: string | null;
  startedAt?: number;
  skipped?: string[];
}
interface SyncSource {
  syncing?: Record<string, boolean>;
  counts?: Record<string, number>;
  jobs?: Record<string, SyncJobLike>;
}

interface SaveRecord {
  count: number;
  ts: number;
}
interface SaveSource {
  active?: boolean;
  lastSave?: SaveRecord | null;
}

interface WebJobLike {
  status?: string;
  phase?: string;
  postId?: string | null;
  key?: string;
  domain?: string;
  finalUrl?: string;
  url?: string;
  stage?: string;
  error?: string;
  progress?: number;
  partial?: boolean;
}
interface WebSource {
  jobs?: WebJobLike[];
}

interface UpdateStateShape {
  status: string;
  version?: string | null;
  progress?: number | null;
  log?: string | null;
  error?: string | null;
}
interface UpdateSource {
  state?: UpdateStateShape | null;
}

interface BinariesProgressShape {
  phase?: string;
  fraction?: number | null;
  error?: string | null;
}
interface BinariesSource {
  progress?: BinariesProgressShape | null;
}

interface SttProgressShape {
  id?: string;
  progress?: number | null;
  label?: string | null;
}
interface SttSource {
  progress?: SttProgressShape | null;
}

// The full source bag consumed by buildActivities / the provider.
export interface ActivitySources {
  analysis?: AnalysisSource;
  downloads?: DownloadsSource;
  model?: ModelSource;
  sync?: SyncSource;
  save?: SaveSource;
  update?: UpdateSource;
  binaries?: BinariesSource;
  stt?: SttSource;
  web?: WebSource;
}

// Stati semantici unificati → etichetta mostrata, risolta nella lingua attiva via
// la chiave piatta `status_<stato>` così ogni superficie (strip, popover) legge
// le stesse parole. `t` è il binder di namespace 'activity'.
export function statusLabel(status: string, t: Translate): string {
  return t('status_' + status);
}

// Quanto "merita attenzione" un kind: gli item che richiedono un'azione o sono in
// errore salgono in cima alla strip e dettano l'headline.
const KIND_PRIORITY: Record<ActivityKind, number> = {
  update: 0,
  model: 1,
  analysis: 2,
  download: 3,
  web: 4,
  save: 5,
  sync: 6,
  binaries: 7,
  stt: 8,
};

// Nomi commerciali delle piattaforme: brand, non testo traducibile.
const PLATFORM_LABEL: Record<string, string> = {
  instagram: 'Instagram',
  twitter: 'X',
  pinterest: 'Pinterest',
};

// Fasi note della pipeline di cattura siti (F10); l'etichetta mostrata è risolta
// nella lingua attiva via la chiave piatta `phase_<fase>`.
const WEB_PHASES = new Set<string>([
  'queued',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
  'done',
  'error',
]);
function webPhaseLabel(phase: string | undefined, t: Translate): string | undefined {
  return phase != null && WEB_PHASES.has(phase) ? t('phase_' + phase) : undefined;
}
const webKey = (j: WebJobLike): string | undefined => (j?.postId ? `web:${j.postId}` : j?.key);
function hostFrom(j: WebJobLike, t: Translate): string {
  if (j?.domain) return j.domain;
  try {
    return new URL((j?.finalUrl || j?.url) as string).hostname.replace(/^www\./, '');
  } catch {
    return t('hostFallback');
  }
}

function pct(progress: number | null | undefined): number | null {
  return progress != null && isFinite(progress) ? Math.round(progress * 100) : null;
}

// Etichetta compatta per la strip/headline ("Auto-tag 3/12", "Download 2/8", …).
function shortLabel(item: ActivityItem, t: Translate): string {
  const p = pct(item.progress);
  switch (item.kind) {
    case 'analysis':
      return t('shortAnalysis', { count: item.count ?? 0, total: item.total ?? 0 });
    case 'download':
      return t('shortDownload', { count: item.count ?? 0, total: item.total ?? 0 });
    case 'web':
      return t('shortWeb');
    case 'save':
      return t('shortSave');
    case 'sync':
      return t('shortSync', {
        platform: PLATFORM_LABEL[item.platform ?? ''] || item.platform || '',
      }).trim();
    case 'model':
      return p != null ? t('shortModelPct', { pct: p }) : t('shortModel');
    case 'stt':
      return p != null ? t('shortSttPct', { pct: p }) : t('shortStt');
    case 'binaries':
      return p != null ? t('shortBinariesPct', { pct: p }) : t('shortBinaries');
    case 'update':
      return item.title || t('shortUpdate');
    default:
      return item.title || '';
  }
}

// ── Normalizzazione updater ─────────────────────────────────────────────────────
// L'updater è sempre "vivo" finché c'è qualcosa da mostrare/azionare: gli stati
// in corso hanno una progress, quelli azionabili portano una CTA.
const UPDATE_SHOWN = [
  'available',
  'downloading',
  'building',
  'built',
  'installing',
  'downloaded',
  'manual',
  'error',
];
function updateItem(state: UpdateStateShape | null | undefined, t: Translate): ActivityItem | null {
  if (!state || !UPDATE_SHOWN.includes(state.status)) return null;
  const { status, version, progress, log, error } = state;
  const v = version ? ` ${version}` : '';
  const base = { id: 'update', kind: 'update' as const, version: version ?? null, postId: null };
  switch (status) {
    case 'available':
      return {
        ...base,
        status: 'available',
        title: t('updateAvailable', { v }),
        subtitle: t('updateAvailableSub'),
        progress: null,
        actions: [
          { id: 'update-rebuild', label: t('updateNow'), variant: 'accent' },
          { id: 'update-dismiss', label: t('updateLater'), variant: 'ghost' },
        ],
      };
    case 'downloading':
      return {
        ...base,
        status: 'running',
        title: t('updateDownloading'),
        subtitle: pct(progress) != null ? `${version || ''} — ${pct(progress)}%` : version,
        progress: progress ?? null,
      };
    case 'building':
      return {
        ...base,
        status: 'running',
        title: t('updateBuilding'),
        subtitle: log || t('updateBuildingSub', { v }),
        progress: null,
      };
    case 'installing':
      return {
        ...base,
        status: 'installing',
        title: t('updateInstalling'),
        subtitle: t('updateInstallingSub'),
        progress: null,
      };
    case 'built':
    case 'downloaded':
      return {
        ...base,
        status: 'ready',
        title: t('updateReady', { v }),
        subtitle: t('updateReadySub'),
        progress: null,
        actions: [
          { id: 'update-install', label: t('updateRestartNow'), variant: 'accent' },
          { id: 'update-dismiss', label: t('updateLater'), variant: 'ghost' },
        ],
      };
    case 'manual':
      return {
        ...base,
        status: 'available',
        title: t('updateAvailable', { v }),
        subtitle: t('updateManualSub'),
        progress: null,
        actions: [
          { id: 'update-download', label: t('updateDownload'), variant: 'accent' },
          { id: 'update-dismiss', label: t('updateLater'), variant: 'ghost' },
        ],
      };
    case 'error':
      return {
        ...base,
        status: 'error',
        title: t('updateFailed'),
        subtitle: error || t('updateFailedSub'),
        progress: null,
        actions: [
          // "Riprova" → updater.rebuildNow(): su Windows ritenta il self-rebuild,
          // su macOS degrada a un nuovo check (reset + checkNow) che ri-espone la
          // CTA di download — non è mai un no-op.
          { id: 'update-rebuild', label: t('updateRetry'), variant: 'accent' },
          { id: 'update-dismiss', label: t('updateClose'), variant: 'ghost' },
        ],
      };
    default:
      return null;
  }
}

// ── Selettore puro ──────────────────────────────────────────────────────────────
// sources = {
//   analysis: { jobs, concurrency, paused },
//   downloads: { jobs, isPaused },
//   model: { status, progress },
//   sync: { syncing: {instagram,twitter}, counts: {instagram,twitter},
//           jobs: {instagram: {status, currentLabel, stepIndex, stepCount,
//                              scanned, fresh, error, …}} },
//   save: { active },
//   update: { state },
//   binaries: { progress: {phase, fraction} },
//   stt: { progress: {id, progress, label} },
// }
export function buildActivities(sources: ActivitySources = {}, lang = 'it'): BuildActivitiesResult {
  const t: Translate = (k, vars) => translate(lang, 'activity.' + k, vars);
  const live: ActivityItem[] = [];
  const { analysis, downloads, model, sync, save, update, binaries, stt, web } = sources;

  // Auto-tag (analisi VLM) — un solo item aggregato; il dettaglio per-post vive
  // nella tab dedicata "AI Tags".
  if (analysis?.jobs) {
    // Riusa la summary già calcolata dal provider se presente; i chiamanti puri
    // (test) la omettono, quindi in quel caso la ricalcoliamo come prima.
    const sum = analysis.summary || analysisSummary(analysis.jobs, analysis.concurrency);
    if (sum.active) {
      const total = sum.total || 0;
      live.push({
        id: 'analysis',
        kind: 'analysis',
        status: analysis.paused ? 'paused' : 'running',
        title: t('analysisTitle'),
        subtitle: sum.current || null,
        count: sum.done,
        total,
        progress: total ? sum.done / total : null,
        etaMs: sum.etaMs,
        actions: [
          {
            id: 'analysis-toggle',
            label: analysis.paused ? t('actionResume') : t('actionPause'),
            variant: 'ghost',
          },
          { id: 'analysis-cancel', label: t('actionCancelAll'), variant: 'danger' },
        ],
      });
    }
  }

  // Download media — un item aggregato con progress complessiva della coda.
  if (downloads?.jobs?.length) {
    const jobs = downloads.jobs;
    const done = jobs.filter((j) => j.status === 'done').length;
    const running = jobs.filter((j) => j.status === 'downloading');
    const queued = jobs.filter((j) => j.status === 'pending').length;
    if (running.length || queued) {
      const total = jobs.filter((j) => j.status !== 'cancelled').length;
      const partial = running.reduce((s, j) => s + (j.progress || 0), 0);
      live.push({
        id: 'download',
        kind: 'download',
        status: downloads.isPaused ? 'paused' : 'running',
        title: t('downloadTitle'),
        subtitle: running[0]?.authorUsername ? `@${running[0].authorUsername}` : null,
        count: done,
        total,
        progress: total ? (done + partial) / total : null,
        actions: [
          {
            id: 'download-toggle',
            label: downloads.isPaused ? t('actionResume') : t('actionPause'),
            variant: 'ghost',
          },
          { id: 'download-cancel', label: t('actionCancelAll'), variant: 'danger' },
        ],
      });
    }
  }

  // Cattura siti web (F10): un item per job in corso, con la fase corrente in
  // chiaro così l'utente segue cosa sta succedendo dietro le quinte.
  if (web?.jobs?.length) {
    for (const j of web.jobs) {
      const st = j.status || j.phase;
      if (st === 'done' || st === 'cancelled') continue; // i conclusi vanno nello storico
      const isErr = st === 'error';
      const host = hostFrom(j, t);
      const detail = j.stage || webPhaseLabel(j.phase, t) || t('inProgress');
      live.push({
        id: webKey(j) ?? '',
        kind: 'web',
        postId: j.postId ?? null,
        status: isErr ? 'error' : 'running',
        title: isErr ? t('webTitleError') : t('webTitle'),
        subtitle: isErr ? j.error || host : `${host} · ${detail}`,
        progress: typeof j.progress === 'number' ? j.progress : null,
        actions: isErr
          ? [
              { id: 'web-retry', label: t('actionRetry'), variant: 'ghost' },
              { id: 'web-cancel', label: t('actionRemove'), variant: 'danger' },
            ]
          : [{ id: 'web-cancel', label: t('actionCancel'), variant: 'danger' }],
      });
    }
  }

  // Download del modello VLM (analyze).
  const modelDownloading = !!model?.progress || !!model?.status?.downloading;
  if (modelDownloading) {
    const prog = model?.progress?.progress ?? null;
    live.push({
      id: 'model',
      kind: 'model',
      status: 'running',
      title: t('modelTitle'),
      subtitle: model?.progress?.label || null,
      progress: prog,
    });
  }

  // Sync di una source dalla Libreria (run sequenziale in background): un item
  // ricco per piattaforma — cartella corrente, passo X/Y, contatori live, stop —
  // che SOSTITUISCE l'item legacy della stessa piattaforma. Un run fallito per
  // login/navigazione resta come item d'errore azionabile (apri browser /
  // chiudi) finché non viene archiviato.
  const syncJobs = sync?.jobs || {};
  for (const platform of Object.keys(syncJobs)) {
    const j = syncJobs[platform];
    if (!j) continue;
    const platformLabel = PLATFORM_LABEL[platform] || platform;
    if (j.status === 'navigating' || j.status === 'syncing') {
      const parts = [j.currentLabel || t('syncAllSaved')];
      if ((j.stepCount ?? 0) > 1) parts.push(`${(j.stepIndex ?? 0) + 1}/${j.stepCount}`);
      parts.push(t('syncCounts', { scanned: j.scanned || 0, n: j.fresh || 0 }));
      live.push({
        id: `sync:${platform}`,
        kind: 'sync',
        platform,
        status: 'running',
        title: t('syncTitle', { platform: platformLabel }),
        subtitle: parts.join(' · '),
        count: j.fresh || 0,
        progress: null,
        actions: [{ id: 'sourcesync-stop', label: t('actionStop'), variant: 'danger' }],
      });
    } else if (j.status === 'error') {
      const isLogin = j.error === 'login';
      live.push({
        id: `sync:${platform}:error`,
        kind: 'sync',
        platform,
        status: 'error',
        title: isLogin
          ? t('syncLoginTitle', { platform: platformLabel })
          : t('syncErrorTitle', { platform: platformLabel }),
        subtitle: isLogin ? t('syncLoginSub') : t('syncErrorSub'),
        progress: null,
        actions: [
          { id: 'sourcesync-open', label: t('syncOpenBrowser'), variant: 'accent' },
          { id: 'sourcesync-dismiss', label: t('actionClose'), variant: 'ghost' },
        ],
      });
    }
  }

  // Sincronizzazione raccolte social — un item per piattaforma in sync
  // (avviata manualmente dal Browser; le piattaforme già coperte da un job
  // source-sync attivo sono saltate per non duplicare la riga).
  if (sync?.syncing) {
    for (const platform of Object.keys(sync.syncing)) {
      if (!sync.syncing[platform]) continue;
      const j = syncJobs[platform];
      if (j && (j.status === 'navigating' || j.status === 'syncing')) continue;
      const n = sync.counts?.[platform] || 0;
      live.push({
        id: `sync:${platform}`,
        kind: 'sync',
        platform,
        status: 'running',
        title: t('syncTitle', { platform: PLATFORM_LABEL[platform] || platform }),
        subtitle: n ? t('newPosts', { n }) : t('inProgress'),
        count: n,
        progress: null,
      });
    }
  }

  // Salvataggio post selezionati dal Browser.
  if (save?.active) {
    live.push({
      id: 'save',
      kind: 'save',
      status: 'running',
      title: t('saveTitle'),
      subtitle: t('saveSubtitle'),
      progress: null,
    });
  }

  // Provisioning binari (yt-dlp / ffmpeg / llama-server).
  const bphase = binaries?.progress?.phase;
  if (bphase && bphase !== 'done' && bphase !== 'error') {
    live.push({
      id: 'binaries',
      kind: 'binaries',
      status: 'running',
      title: t('binariesTitle'),
      subtitle: bphase,
      progress: binaries?.progress?.fraction ?? null,
    });
  }

  // Download del modello speech-to-text.
  const sttProg = stt?.progress?.progress;
  if (sttProg != null && sttProg < 1) {
    live.push({
      id: 'stt',
      kind: 'stt',
      status: 'running',
      title: t('sttTitle'),
      subtitle: stt?.progress?.label || null,
      progress: sttProg,
    });
  }

  // Aggiornamento OTA.
  const upd = updateItem(update?.state, t);
  if (upd) live.push(upd);

  // Errori in cima, poi per priorità di kind. Stabile rispetto all'ordine d'inserimento.
  live.sort((a, b) => {
    const ea = a.status === 'error' ? 0 : 1;
    const eb = b.status === 'error' ? 0 : 1;
    if (ea !== eb) return ea - eb;
    return (KIND_PRIORITY[a.kind] ?? 9) - (KIND_PRIORITY[b.kind] ?? 9);
  });

  for (const it of live) {
    const base = shortLabel(it, t);
    it.short = it.status === 'paused' ? `${base} · ${t('pausedSuffix')}` : base;
  }

  const hasError = live.some((i) => i.status === 'error');
  const needsAction = live.some((i) => i.status === 'available' || i.status === 'ready');
  const primary = live[0] || null;
  const headline = !live.length
    ? t('headlineIdle')
    : live.length === 1
      ? (primary.short ?? '')
      : t('headlineMore', { primary: primary.short, n: live.length - 1 });

  return {
    live,
    summary: {
      activeCount: live.length,
      headline,
      hasError,
      needsAction,
      // La barra della strip segue l'item primario (null = niente barra/indeterminato).
      progress: primary ? (primary.progress ?? null) : null,
      primaryKind: primary ? primary.kind : null,
      // Stato dell'item primario: la strip ferma spinner/pulse quando è 'paused'.
      primaryStatus: primary ? primary.status : null,
    },
  };
}

// ── Storico di sessione ─────────────────────────────────────────────────────────
const LOG_CAP = 50;

// Input accepted by pushLog: a partial log entry (id/ts/read are filled in).
interface PushLogEntry {
  id?: string;
  kind: ActivityKind | string;
  title: string;
  subtitle?: string | null;
  status: string;
  ts?: number;
  read?: boolean;
  platform?: string;
}

// The context value exposed by useActivity().
export interface ActivityContextValue {
  live: ActivityItem[];
  summary: ActivitySummary;
  log: LogEntry[];
  unread: number;
  pushLog: (entry: PushLogEntry) => void;
  dismiss: (id: string) => void;
  clearAll: () => void;
  markAllRead: () => void;
  dismissUpdate: () => void;
}

// ── Context ──────────────────────────────────────────────────────────────────────
const ActivityContext = createContext<ActivityContextValue | null>(null);

// Per-source edge-detect bookkeeping kept across renders in a single ref.
interface PrevTracker {
  updMilestone?: string | null;
  binPhase?: string | null;
  sttDone?: boolean;
  modelReady?: boolean;
  analysisActive?: boolean;
  analysisDone?: number;
  downloadActive?: boolean;
  webStatus?: Record<string, string>;
  saveTs?: number;
  syncJobs?: Record<string, SyncJobLike>;
  syncing?: Record<string, boolean>;
  syncPeak?: Record<string, number>;
}

// Owns the cross-cutting subscriptions (updater / binaries / stt) and the session
// log; merges them with the lifted live state passed by App.
function useActivityCore(sources: ActivitySources): ActivityContextValue {
  const { lang } = useLang();
  const t = useCallback<Translate>((k, vars) => translate(lang, 'activity.' + k, vars), [lang]);
  const [updateState, setUpdateState] = useState<UpdateStateShape | null>(null);
  const [binariesProgress, setBinariesProgress] = useState<BinariesProgressShape | null>(null);
  const [sttProgress, setSttProgress] = useState<SttProgressShape | null>(null);
  const [log, setLog] = useState<LogEntry[]>([]); // storico { id, kind, title, subtitle, status, ts, read }
  const [updateDismissed, setUpdateDismissed] = useState<string>('');
  // Epoca monotona dell'updater: cresce a ogni evento ricevuto dal main. Entra
  // nella chiave di dismiss così due eventi con stesso status e version === null
  // (es. due 'error' consecutivi) non condividono la stessa chiave: dopo aver
  // chiuso il primo, il secondo riappare invece di essere soppresso.
  const [updateEpoch, setUpdateEpoch] = useState<number>(0);
  const applyUpdateState = useCallback((s: UpdateStateShape | null): void => {
    setUpdateState(s);
    setUpdateEpoch((n) => n + 1);
  }, []);

  // pushLog: prepend con dedup per id (l'ultimo vince), cap a LOG_CAP.
  const idSeq = useRef<number>(0);
  const pushLog = useCallback((entry: PushLogEntry): void => {
    const id = entry.id || `log-${idSeq.current++}`;
    const ts = entry.ts || Date.now();
    setLog((prev) =>
      [{ read: false, ...entry, id, ts } as LogEntry, ...prev.filter((e) => e.id !== id)].slice(
        0,
        LOG_CAP,
      ),
    );
  }, []);
  const dismiss = useCallback(
    (id: string): void => setLog((prev) => prev.filter((e) => e.id !== id)),
    [],
  );
  const clearAll = useCallback((): void => setLog([]), []);
  const markAllRead = useCallback(
    (): void => setLog((prev) => prev.map((e) => (e.read ? e : { ...e, read: true }))),
    [],
  );

  // Sottoscrizioni proprie: updater, binaries, stt.
  useEffect(() => {
    const api = window.electronAPI;
    if (!api) return undefined;
    api
      .getUpdateState?.()
      .then((s) => {
        if (s) applyUpdateState(s as UpdateStateShape);
      })
      .catch(() => {});
    const offUpd = api.onUpdaterState?.((s) => applyUpdateState(s as UpdateStateShape));
    const offBin = api.onBinariesProgress?.((p) => setBinariesProgress(p as BinariesProgressShape));
    const offStt = api.onSttModelProgress?.((p) => setSttProgress(p as SttProgressShape));
    return () => {
      offUpd?.();
      offBin?.();
      offStt?.();
    };
  }, [applyUpdateState]);

  // Edge-detect dei completamenti → storico. Confronta lo stato attuale col
  // precedente e logga le transizioni notevoli.
  const prev = useRef<PrevTracker>({});
  // updater milestone — edge-detect su prev.current: updateState non viene mai
  // azzerato dopo il completamento, quindi senza guard un cambio lingua (che muta
  // l'identità di `t`) rieseguirebbe l'effetto e ri-pubblicherebbe la milestone
  // (notifica già letta che torna non letta e risale in cima allo storico).
  useEffect(() => {
    if (!updateState) return;
    const { status, version, error } = updateState;
    // Include the monotonic updateEpoch so the key changes on every REAL event from
    // the main (each bumps the epoch), while a mere re-render from a language change
    // (same epoch) is still suppressed. Keying on version/error content alone would
    // also swallow a legitimately repeated identical event (e.g. two 'error's).
    const ms =
      status === 'built' || status === 'downloaded'
        ? `ready:${version || ''}:${updateEpoch}`
        : status === 'error'
          ? `error:${updateEpoch}`
          : null;
    if (ms && prev.current.updMilestone !== ms) {
      if (status === 'built' || status === 'downloaded')
        pushLog({
          id: 'log-update-ready',
          kind: 'update',
          status: 'ready',
          title: t('logUpdateReady', { version: version || '' }).trim(),
          subtitle: t('logUpdateReadySub'),
        });
      else
        pushLog({
          id: 'log-update-error',
          kind: 'update',
          status: 'error',
          title: t('logUpdateError'),
          subtitle: error || '',
        });
    }
    prev.current.updMilestone = ms;
  }, [updateState, updateEpoch, pushLog, t]);
  // binaries — "pronti" è informativo (read by default); l'errore resta non letto.
  // Edge-detect su prev.current: binariesProgress resta su 'done'/'error' anche dopo
  // il completamento, quindi senza guard un cambio lingua ri-pubblicherebbe la voce.
  useEffect(() => {
    const phase = binariesProgress?.phase;
    const ms = phase === 'done' ? 'done' : phase === 'error' ? 'error' : null;
    if (ms && prev.current.binPhase !== ms) {
      if (phase === 'done')
        pushLog({
          id: 'log-binaries',
          kind: 'binaries',
          status: 'done',
          title: t('logBinariesReady'),
          read: true,
        });
      else
        pushLog({
          id: 'log-binaries',
          kind: 'binaries',
          status: 'error',
          title: t('logBinariesError'),
          subtitle: binariesProgress?.error || '',
        });
    }
    // Re-arm on any active phase (download/extract/…): a fresh provisioning run
    // passes through one before its terminal event, so the next done/error logs
    // even when it repeats the previous terminal phase (e.g. a retry that fails
    // again with 'error'). A language-change re-render carries no active phase.
    prev.current.binPhase = phase && phase !== 'done' && phase !== 'error' ? null : ms;
  }, [binariesProgress, pushLog, t]);
  // stt ready — informativo (read by default). Logga solo sulla transizione
  // <1 → >=1 via prev.current, così un secondo completamento è loggabile pulito.
  useEffect(() => {
    const sttDone = sttProgress?.progress != null && sttProgress.progress >= 1;
    if (sttDone && !prev.current.sttDone)
      pushLog({
        id: 'log-stt',
        kind: 'stt',
        status: 'done',
        title: t('logSttReady'),
        read: true,
      });
    prev.current.sttDone = sttDone;
  }, [sttProgress, pushLog, t]);

  // model ready: status.ready false→true — informativo (read by default).
  const modelReady = !!sources?.model?.status?.ready;
  useEffect(() => {
    if (modelReady && prev.current.modelReady === false)
      pushLog({
        id: 'log-model',
        kind: 'model',
        status: 'done',
        title: t('logModelReady'),
        read: true,
      });
    prev.current.modelReady = modelReady;
  }, [modelReady, pushLog, t]);

  // analysis batch done: active>0 → 0 con almeno un completato.
  // La summary è calcolata una sola volta qui e riusata sia dall'effetto sia da
  // buildActivities (via liveSources), evitando il doppio analysisSummary.
  const analysisJobs = sources?.analysis?.jobs;
  const analysisConcurrency = sources?.analysis?.concurrency;
  const analysisSum = useMemo(
    () => analysisSummary(analysisJobs || [], analysisConcurrency),
    [analysisJobs, analysisConcurrency],
  );
  useEffect(() => {
    const sum = analysisSum;
    const wasActive = prev.current.analysisActive;
    if (wasActive && !sum.active && sum.done > 0)
      pushLog({
        id: 'log-analysis-done',
        kind: 'analysis',
        status: 'done',
        title: t('logAnalysisDone'),
        subtitle: t('logAnalysisDoneSub', { n: sum.done }),
      });
    prev.current.analysisActive = sum.active;
    prev.current.analysisDone = sum.done;
  }, [analysisSum, pushLog, t]);

  // download batch done: coda attiva → vuota
  const downloadJobs = sources?.downloads?.jobs;
  useEffect(() => {
    const jobs = downloadJobs || [];
    const active = jobs.some((j) => j.status === 'downloading' || j.status === 'pending');
    const done = jobs.filter((j) => j.status === 'done').length;
    if (prev.current.downloadActive && !active && done > 0)
      pushLog({
        id: 'log-download-done',
        kind: 'download',
        status: 'done',
        title: t('logDownloadDone'),
        subtitle: t('logDownloadDoneSub', { n: done }),
      });
    prev.current.downloadActive = active;
  }, [downloadJobs, pushLog, t]);

  // cattura sito: transizione di fase → done/error nello storico.
  const webJobs = sources?.web?.jobs;
  useEffect(() => {
    const jobs = webJobs || [];
    const prevMap = prev.current.webStatus || {};
    const next: Record<string, string> = {};
    for (const j of jobs) {
      const k = j.postId ? `web:${j.postId}` : j.key;
      if (!k) continue;
      const st = j.status || j.phase;
      if (!st) continue;
      next[k] = st;
      if (prevMap[k] && prevMap[k] !== st) {
        const host =
          j.domain ||
          (() => {
            try {
              return new URL((j.finalUrl || j.url) as string).hostname.replace(/^www\./, '');
            } catch {
              return '';
            }
          })();
        if (st === 'done')
          pushLog({
            id: `log-${k}`,
            kind: 'web',
            status: 'done',
            title: j.partial ? t('logWebAddedPartial') : t('logWebAdded'),
            subtitle: host,
          });
        else if (st === 'error')
          pushLog({
            id: `log-${k}`,
            kind: 'web',
            status: 'error',
            title: t('logWebError'),
            subtitle: j.error || host,
          });
      }
    }
    prev.current.webStatus = next;
  }, [webJobs, pushLog, t]);

  // salvataggio selezionati completato → "N post salvati"
  const lastSave = sources?.save?.lastSave;
  useEffect(() => {
    if (lastSave && lastSave.ts !== prev.current.saveTs) {
      if (lastSave.count > 0)
        pushLog({
          id: `log-save-${lastSave.ts}`,
          kind: 'save',
          status: 'done',
          title: t('logSaveDone'),
          subtitle: t('logSaveDoneSub', { n: lastSave.count }),
        });
      prev.current.saveTs = lastSave.ts;
    }
  }, [lastSave, pushLog, t]);

  // source-sync (Libreria) → storico: il job porta i totali finali del run, quindi
  // niente peak-tracking. Logga la transizione attivo → done/stopped/error; gli
  // errori restano ANCHE come item live azionabile finché non vengono archiviati.
  const syncJobs = sources?.sync?.jobs;
  useEffect(() => {
    const cur = syncJobs || {};
    const prevJobs = prev.current.syncJobs || {};
    for (const platform of Object.keys(cur)) {
      const now = cur[platform];
      const was = prevJobs[platform];
      const wasActive = was && (was.status === 'navigating' || was.status === 'syncing');
      if (!now || !wasActive) continue;
      const label = PLATFORM_LABEL[platform] || platform;
      const id = `log-sourcesync-${platform}-${now.startedAt}`;
      if (now.status === 'done') {
        pushLog({
          id,
          kind: 'sync',
          status: 'done',
          platform,
          title: t('logSyncDone', { platform: label }),
          subtitle: now.skipped?.length
            ? t('logSyncDoneSkippedSub', { n: now.fresh || 0, m: now.skipped.length })
            : t('logSyncDoneSub', { n: now.fresh || 0 }),
        });
      } else if (now.status === 'stopped') {
        pushLog({
          id,
          kind: 'sync',
          status: 'done',
          platform,
          title: t('logSyncStopped', { platform: label }),
          subtitle: t('logSyncDoneSub', { n: now.fresh || 0 }),
        });
      } else if (now.status === 'error') {
        pushLog({
          id,
          kind: 'sync',
          status: 'error',
          platform,
          title:
            now.error === 'login'
              ? t('syncLoginTitle', { platform: label })
              : t('syncErrorTitle', { platform: label }),
          subtitle: now.error === 'login' ? t('syncLoginSub') : t('syncErrorSub'),
        });
      }
    }
    prev.current.syncJobs = { ...cur };
  }, [syncJobs, pushLog, t]);

  // sync done: per-platform true→false → "N nuovi post"
  const syncing = sources?.sync?.syncing;
  const syncCounts = sources?.sync?.counts;
  useEffect(() => {
    const cur = syncing || {};
    const prevSync = prev.current.syncing || {};
    // Picco per-piattaforma del conteggio osservato durante la sessione di sync.
    // newPostsAlert (= syncCounts) viene azzerato da clearAlert() quando l'utente
    // apre il tab in sync: se accade prima del termine, il conteggio "live" cala a
    // 0 e il log perderebbe il dato. Teniamo il massimo visto finché la sync è in
    // corso e lo usiamo al completamento; lo resettiamo all'inizio di una nuova sync.
    const peak = prev.current.syncPeak || (prev.current.syncPeak = {});
    for (const platform of Object.keys(cur)) {
      if (cur[platform] && !prevSync[platform]) peak[platform] = 0; // nuova sync
      if (cur[platform])
        peak[platform] = Math.max(peak[platform] || 0, syncCounts?.[platform] || 0);
    }
    for (const platform of Object.keys(cur)) {
      if (prevSync[platform] && !cur[platform]) {
        // I passi di un source-sync flippano syncing true→false a ogni cartella:
        // lì il completamento lo logga il job (con i totali del run), non questo
        // percorso legacy — che resta per le sync manuali avviate dal Browser.
        const job = syncJobs?.[platform];
        if (job && job.status !== 'error') {
          peak[platform] = 0;
          continue;
        }
        const n = Math.max(peak[platform] || 0, syncCounts?.[platform] || 0);
        pushLog({
          id: `log-sync-${platform}-done`,
          kind: 'sync',
          status: 'done',
          platform,
          title: t('logSyncDone', { platform: PLATFORM_LABEL[platform] || platform }),
          subtitle: n ? t('logSyncDoneSub', { n }) : null,
        });
        peak[platform] = 0;
      }
    }
    prev.current.syncing = { ...cur };
  }, [syncing, syncCounts, syncJobs, pushLog, t]);

  // Lo stato updater dismesso non deve riapparire nella strip live. Dipendiamo
  // dalle singole sorgenti (già memoizzate in App) anziché dall'oggetto `sources`
  // del rest-spread, che cambia identità a ogni render e rifarebbe il memo (e
  // quindi buildActivities) inutilmente.
  const srcAnalysis = sources?.analysis;
  const srcDownloads = sources?.downloads;
  const srcModel = sources?.model;
  const srcSync = sources?.sync;
  const srcSave = sources?.save;
  const srcWeb = sources?.web;
  const liveSources = useMemo<ActivitySources>(
    () => ({
      analysis: { ...srcAnalysis, summary: analysisSum },
      downloads: srcDownloads,
      model: srcModel,
      sync: srcSync,
      save: srcSave,
      web: srcWeb,
      update: {
        state:
          updateState &&
          `${updateState.status}:${updateState.version}:${updateEpoch}` === updateDismissed &&
          !['downloading', 'building', 'installing'].includes(updateState.status)
            ? null
            : updateState,
      },
      binaries: { progress: binariesProgress },
      stt: { progress: sttProgress },
    }),
    [
      srcAnalysis,
      srcDownloads,
      srcModel,
      srcSync,
      srcSave,
      srcWeb,
      analysisSum,
      updateState,
      updateEpoch,
      updateDismissed,
      binariesProgress,
      sttProgress,
    ],
  );

  const { live, summary } = useMemo(() => buildActivities(liveSources, lang), [liveSources, lang]);

  // unread è esportato a parte; non muta summary (immutabilità del memo).
  const unread = useMemo(() => log.filter((e) => !e.read).length, [log]);

  const dismissUpdate = useCallback((): void => {
    if (updateState)
      setUpdateDismissed(`${updateState.status}:${updateState.version}:${updateEpoch}`);
  }, [updateState, updateEpoch]);

  return useMemo(
    () => ({ live, summary, log, unread, pushLog, dismiss, clearAll, markAllRead, dismissUpdate }),
    [live, summary, log, unread, pushLog, dismiss, clearAll, markAllRead, dismissUpdate],
  );
}

interface ActivityProviderProps extends ActivitySources {
  children: ReactNode;
}

export function ActivityProvider({ children, ...sources }: ActivityProviderProps): ReactElement {
  const value = useActivityCore(sources);
  return createElement(ActivityContext.Provider, { value }, children);
}

export function useActivity(): ActivityContextValue {
  const ctx = useContext(ActivityContext);
  if (!ctx) throw new Error('useActivity must be used within an ActivityProvider');
  return ctx;
}
