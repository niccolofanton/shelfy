import React, { useRef, useState } from 'react';
import {
  Activity,
  Sparkles,
  Download,
  Bookmark,
  RefreshCw,
  Wrench,
  Mic,
  ArrowUpCircle,
  Loader2,
  AlertTriangle,
  CheckCircle2,
  X,
  Trash2,
  Globe,
} from 'lucide-react';
import Popover from './Popover';
import { useActivity } from '../hooks/useActivity';
import { useT } from '../i18n';
import { formatEta } from '../lib/duration';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// Navigable views a notification can route to (mirrors App's View union).
type NavView =
  | 'gallery'
  | 'downloads'
  | 'aitags'
  | 'aiqueue'
  | 'aiweb'
  | 'aisearch'
  | 'settings'
  | 'browser';

// The three browser-backed platforms a sync notification can target.
type BrowserPlatform = 'instagram' | 'twitter' | 'pinterest';

// Extra routing params handed alongside a view (platform for sync, postId for web).
interface NavParams {
  platform?: BrowserPlatform;
  postId?: string | null;
}

// The minimal item shape App's queue-control handler reads off a delegated action.
interface ActivityActionItem {
  id?: string;
  platform?: BrowserPlatform;
}

// Lucide-compatible icon component: the subset of props this surface passes.
type IconComponent = React.ComponentType<{
  size?: number | string;
  className?: string;
  strokeWidth?: number | string;
  style?: React.CSSProperties;
}>;

// The background-activity kinds the center knows how to render/route.
type ActivityKind =
  | 'analysis'
  | 'download'
  | 'web'
  | 'save'
  | 'sync'
  | 'model'
  | 'binaries'
  | 'stt'
  | 'update';

// A single action button attached to a live item (cancel/retry/pause/CTA).
interface ActivityAction {
  id: string;
  label: string;
  variant?: 'accent' | 'danger' | 'ghost';
}

// A live ("In corso") activity item, as built by useActivity/buildActivities.
interface LiveItem {
  id: string;
  kind: ActivityKind;
  status: string;
  title: string;
  subtitle?: string | null;
  count?: number;
  total?: number | null;
  progress?: number | null;
  etaMs?: number | null;
  short?: string;
  platform?: BrowserPlatform;
  postId?: string | null;
  actions?: ActivityAction[];
}

// A history ("Recenti") log entry.
interface LogEntry {
  id: string;
  kind: ActivityKind;
  status: string;
  title: string;
  subtitle?: string | null;
  ts: number;
  read?: boolean;
}

// The aggregate strip summary driven by the primary live item.
interface ActivitySummary {
  activeCount: number;
  headline: string;
  hasError: boolean;
  needsAction: boolean;
  progress: number | null;
  primaryKind: ActivityKind | null;
  primaryStatus: string | null;
}

// Icona per ogni tipo di attività — coerente con le icone già usate altrove.
const KIND_ICON: Record<ActivityKind, IconComponent> = {
  analysis: Sparkles,
  download: Download,
  web: Globe,
  save: Bookmark,
  sync: RefreshCw,
  model: Sparkles,
  binaries: Wrench,
  stt: Mic,
  update: ArrowUpCircle,
};

// View di destinazione quando si clicca una notifica (apre la sezione giusta).
// sync → Browser sulla piattaforma giusta; modello/strumenti/voce/aggiornamento
// vivono tutti nelle Impostazioni.
const KIND_VIEW: Partial<Record<ActivityKind, NavView>> = {
  analysis: 'aiqueue',
  download: 'downloads',
  web: 'aiweb',
  save: 'gallery',
  sync: 'browser',
  model: 'settings',
  binaries: 'settings',
  stt: 'settings',
  update: 'settings',
};

const ACCENT = '#7B5CFF';

function pct(progress: number | null | undefined): number | null {
  return progress != null && isFinite(progress) ? Math.round(progress * 100) : null;
}

// "adesso", "3 min fa", "2 h fa" — relativa, compatta. `t` è il binder 'activity'.
function timeAgo(ts: number | null | undefined, t: Translate): string {
  if (!ts) return '';
  const s = Math.max(0, Math.round((Date.now() - ts) / 1000));
  if (s < 45) return t('timeNow');
  const m = Math.round(s / 60);
  if (m < 60) return t('timeMinAgo', { n: m });
  const h = Math.round(m / 60);
  if (h < 24) return t('timeHourAgo', { n: h });
  return t('timeDayAgo', { n: Math.round(h / 24) });
}

// Pallino di stato per le righe dello storico.
function StatusDot({ status }: { status: string }): React.JSX.Element {
  if (status === 'error') return <AlertTriangle size={13} className="text-amber-400 shrink-0" />;
  return <CheckCircle2 size={13} className="text-green-500 shrink-0" />;
}

interface ActionButtonProps {
  action: ActivityAction;
  onClick: () => void;
}

// Pulsante azione (cancel/retry/pausa/CTA updater) coerente con i toast esistenti.
function ActionButton({ action, onClick }: ActionButtonProps): React.JSX.Element {
  const cls =
    action.variant === 'accent'
      ? 'text-black bg-[var(--accent)] hover:opacity-90'
      : action.variant === 'danger'
        ? 'text-gray-400 hover:text-red-400'
        : 'text-gray-400 hover:text-white';
  const padded = action.variant === 'accent' ? 'px-3 py-1 rounded-lg font-medium' : 'px-2 py-1';
  return (
    <button
      data-testid={`activity-action-${action.id}`}
      onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
        e.stopPropagation();
        onClick();
      }}
      className={`u-press u-fade-in text-[11px] transition-colors ${padded} ${cls}`}
    >
      {action.label}
    </button>
  );
}

interface LiveRowProps {
  item: LiveItem;
  index?: number;
  onAction?: (id: string, item: LiveItem) => void;
  onOpen?: (item: LiveItem) => void;
  t: Translate;
}

// Una riga "In corso".
function LiveRow({ item, index = 0, onAction, onOpen, t }: LiveRowProps): React.JSX.Element {
  const Icon = KIND_ICON[item.kind] || Activity;
  const p = pct(item.progress);
  const running = item.status === 'running';
  const isError = item.status === 'error';
  const clickable = !!KIND_VIEW[item.kind];
  return (
    <div
      data-testid={`activity-item-${item.id}`}
      onClick={clickable ? () => onOpen?.(item) : undefined}
      style={{ animationDelay: index * 40 + 'ms' }}
      className={[
        'u-scale-in flex flex-col gap-1.5 px-3 py-2 transition-colors',
        clickable ? 'u-press cursor-pointer hover:bg-[#222]' : '',
      ].join(' ')}
    >
      <div className="flex items-center gap-2.5">
        <span className="shrink-0 flex items-center justify-center w-6 h-6">
          {running && item.progress == null ? (
            <Loader2 size={15} className="animate-spin" style={{ color: ACCENT }} />
          ) : isError ? (
            <AlertTriangle size={15} className="text-amber-400" />
          ) : (
            <Icon size={15} style={{ color: ACCENT }} />
          )}
        </span>
        <div className="flex-1 min-w-0 flex flex-col leading-tight">
          <span className="text-xs text-gray-200 truncate">
            {item.title}
            {item.status === 'paused' ? (
              <span className="text-amber-400"> · {t('status_paused')}</span>
            ) : (
              item.etaMs != null && (
                <span className="text-gray-500"> · {formatEta(item.etaMs)}</span>
              )
            )}
          </span>
          {item.subtitle && (
            <span className="text-[11px] text-gray-500 truncate">{item.subtitle}</span>
          )}
        </div>
        {item.total != null && item.total > 0 && (
          <span className="text-[11px] text-gray-400 tabular-nums shrink-0">
            {item.count ?? 0}/{item.total}
          </span>
        )}
        {p != null && item.total == null && (
          <span
            className="text-[11px] font-semibold tabular-nums shrink-0"
            style={{ color: ACCENT }}
          >
            {p}%
          </span>
        )}
      </div>

      {item.progress != null && (
        <span className="w-full h-1 rounded bg-[#2a2a2a] overflow-hidden block">
          <span
            className="u-progress block h-full"
            style={{ width: `${p}%`, background: ACCENT }}
          />
        </span>
      )}
      {running && item.progress == null && item.kind !== 'sync' && item.kind !== 'save' && (
        <span className="w-full h-1 rounded bg-[#2a2a2a] overflow-hidden block ai-progress-track" />
      )}

      {item.actions && item.actions.length > 0 && (
        <div className="flex items-center gap-1 justify-end">
          {item.actions.map((a) => (
            <ActionButton key={a.id} action={a} onClick={() => onAction?.(a.id, item)} />
          ))}
        </div>
      )}
    </div>
  );
}

interface LogRowProps {
  entry: LogEntry;
  index?: number;
  onOpen?: (entry: LogEntry) => void;
  onDismiss: (id: string) => void;
  t: Translate;
}

// Una riga dello storico ("Recenti").
function LogRow({ entry, index = 0, onOpen, onDismiss, t }: LogRowProps): React.JSX.Element {
  const Icon = KIND_ICON[entry.kind] || Activity;
  const clickable = !!KIND_VIEW[entry.kind];
  return (
    <div
      data-testid={`activity-log-${entry.id}`}
      onClick={clickable ? () => onOpen?.(entry) : undefined}
      style={{ animationDelay: index * 20 + 'ms' }}
      className={[
        'u-fade-in-up group flex items-start gap-2.5 px-3 py-1.5 hover:bg-[#222] transition-colors',
        clickable ? 'u-press cursor-pointer' : '',
      ].join(' ')}
    >
      <span className="shrink-0 flex items-center justify-center w-6 h-6 text-gray-500">
        <Icon size={14} />
      </span>
      <div className="flex-1 min-w-0 flex flex-col leading-tight">
        <span className="text-xs text-gray-300 truncate flex items-center gap-1.5">
          <StatusDot status={entry.status} />
          {entry.title}
        </span>
        {entry.subtitle && (
          <span className="text-[11px] text-gray-500 truncate">{entry.subtitle}</span>
        )}
      </div>
      <span className="text-[10px] text-gray-600 tabular-nums shrink-0 mt-0.5 group-hover:hidden">
        {timeAgo(entry.ts, t)}
      </span>
      <button
        data-testid={`activity-log-dismiss-${entry.id}`}
        onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
          e.stopPropagation();
          onDismiss(entry.id);
        }}
        title={t('actionRemove')}
        className="u-press u-fade-in hidden group-hover:flex items-center justify-center w-5 h-5 rounded text-gray-500 hover:text-white shrink-0"
      >
        <X size={12} />
      </button>
    </div>
  );
}

interface ActivityCenterProps {
  onAction?: (id: string, item?: ActivityActionItem) => void;
  onNavigate?: (view: NavView, params?: NavParams) => void;
}

// Strip in fondo alla sidebar + popover del Centro Attività. Allineata al
// pulsante Impostazioni (gap-3 px-4 py-2.5, icona 16px).
export default function ActivityCenter({
  onAction,
  onNavigate,
}: ActivityCenterProps): React.JSX.Element {
  const t: Translate = useT('activity');
  // The activity hook aggregates several parallel IPC subscriptions; its exact
  // surface is consumed here through the local activity shapes (LiveItem/LogEntry
  // /ActivitySummary) which mirror what buildActivities produces.
  const { live, summary, log, unread, dismiss, clearAll, markAllRead, dismissUpdate } =
    useActivity() as unknown as {
      live: LiveItem[];
      summary: ActivitySummary;
      log: LogEntry[];
      unread: number;
      dismiss: (id: string) => void;
      clearAll: () => void;
      markAllRead: () => void;
      dismissUpdate: () => void;
    };
  const [open, setOpen] = useState<boolean>(false);
  const anchorRef = useRef<HTMLButtonElement | null>(null);

  const busy = summary.activeCount > 0;
  const paused = summary.primaryStatus === 'paused';
  // Un item "pronto"/"disponibile" (es. aggiornamento OTA scaricato) ha progress
  // null ma NON è in lavorazione: va mostrato come azione richiesta, non come
  // spinner perpetuo — altrimenti la striscia sembra restare "in pending" per
  // sempre invece di segnalare che il download è finito.
  const needsAction = summary.needsAction;
  const working =
    busy &&
    !paused &&
    !needsAction &&
    (summary.primaryStatus === 'running' || summary.primaryStatus === 'installing');
  const StripIcon =
    busy && summary.primaryKind ? KIND_ICON[summary.primaryKind] || Activity : Activity;
  const stripBarPct = pct(summary.progress);

  const toggle = (): void => {
    // Mark the log read when opening. `open` is the pre-toggle value in this click
    // handler — keep the setState out of the setOpen updater (that runs in render).
    if (!open && unread) markAllRead();
    setOpen((o) => !o);
  };

  // Le azioni dell'updater vivono qui (stesse IPC del vecchio toast); le altre
  // (analisi/download) sono delegate ad App via onAction.
  const handleAction = (id: string, item: LiveItem): void => {
    switch (id) {
      case 'update-rebuild':
        window.electronAPI?.rebuildUpdate?.();
        break;
      case 'update-install':
        window.electronAPI?.quitAndInstallUpdate?.();
        break;
      case 'update-download':
        window.electronAPI?.openUpdateDownload?.();
        break;
      case 'update-dismiss':
        dismissUpdate();
        break;
      default:
        onAction?.(id, item);
    }
  };

  // Click su una notifica → apre la sezione pertinente e chiude il popover.
  // Per il sync passiamo la piattaforma così App apre il sotto-tab giusto del Browser.
  const navigate = (item: LiveItem | LogEntry): void => {
    const view = KIND_VIEW[item.kind];
    if (!view) return;
    if (item.kind === 'sync') onNavigate?.(view, { platform: (item as LiveItem).platform });
    else if (item.kind === 'web') onNavigate?.(view, { postId: (item as LiveItem).postId });
    else onNavigate?.(view);
    setOpen(false);
  };

  return (
    <div className="mx-2 mb-0.5">
      <button
        ref={anchorRef}
        data-testid="activity-strip"
        onClick={toggle}
        title={t('title')}
        className={[
          'u-press w-full flex items-center gap-3 px-4 py-2.5 rounded-md text-sm transition-colors text-left',
          busy
            ? 'text-gray-200 hover:bg-[#1a1a1a]'
            : 'text-gray-500 hover:bg-[#1a1a1a] hover:text-gray-300',
        ].join(' ')}
      >
        <span className="shrink-0 flex items-center justify-center w-4 h-4">
          {summary.hasError ? (
            <AlertTriangle size={16} className="text-amber-400" />
          ) : working && summary.progress == null ? (
            <Loader2 size={16} className="animate-spin" style={{ color: ACCENT }} />
          ) : (
            <StripIcon
              size={16}
              strokeWidth={1.75}
              style={busy ? { color: ACCENT } : undefined}
              className={
                needsAction ? 'u-glow animate-pulse' : busy && !paused ? 'animate-pulse' : ''
              }
            />
          )}
        </span>
        <span className="flex-1 min-w-0 flex flex-col leading-tight">
          <span className="truncate">{t('title')}</span>
          <span className="text-[11px] text-gray-500 truncate">{summary.headline}</span>
        </span>
        {busy && (
          <span
            data-testid="activity-badge"
            className="u-pop-in flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full text-white text-[10px] font-semibold leading-none tabular-nums shrink-0"
            style={{ background: ACCENT }}
          >
            {summary.activeCount}
          </span>
        )}
        {!busy && unread > 0 && (
          <span
            data-testid="activity-unread"
            className="u-pop-in u-glow w-2 h-2 rounded-full shrink-0"
            style={{ background: ACCENT, color: ACCENT }}
          />
        )}
      </button>

      {stripBarPct != null && busy && (
        <span
          data-testid="activity-strip-bar"
          className="mx-1 mt-0.5 h-1 rounded bg-[#2a2a2a] overflow-hidden block"
        >
          <span
            className="u-progress block h-full"
            style={{ width: `${stripBarPct}%`, background: ACCENT }}
          />
        </span>
      )}

      <Popover
        anchorRef={anchorRef}
        open={open}
        onRequestClose={() => setOpen(false)}
        placement="top"
        gap={6}
        data-testid="activity-popover"
        className="u-fade-in-up w-[300px] bg-[#1a1a1a] border border-[#2e2e2e] rounded-lg shadow-2xl flex flex-col max-h-[70vh]"
      >
        <div className="flex items-center justify-between px-3 py-2 border-b border-[#2e2e2e]">
          <p className="text-[10px] font-semibold uppercase tracking-widest text-gray-500">
            {t('title')}
          </p>
          {log.length > 0 && (
            <button
              data-testid="activity-clear-all"
              onClick={clearAll}
              className="u-press flex items-center gap-1 text-[11px] text-gray-500 hover:text-red-400 transition-colors"
            >
              <Trash2 size={12} /> {t('clearAll')}
            </button>
          )}
        </div>

        <div className="overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]">
          {live.length === 0 && log.length === 0 && (
            <p className="px-3 py-4 text-xs text-gray-600 text-center">{t('empty')}</p>
          )}

          {live.length > 0 && (
            <div data-testid="activity-live" className="flex flex-col py-1">
              <p className="px-3 py-1 text-[10px] uppercase tracking-wider text-gray-600">
                {t('sectionLive')}
              </p>
              {live.map((item, i) => (
                <LiveRow
                  key={item.id}
                  item={item}
                  index={i}
                  onAction={handleAction}
                  onOpen={navigate}
                  t={t}
                />
              ))}
            </div>
          )}

          {log.length > 0 && (
            <div
              data-testid="activity-recent"
              className="flex flex-col py-1 border-t border-[#2e2e2e]"
            >
              <p className="px-3 py-1 text-[10px] uppercase tracking-wider text-gray-600">
                {t('sectionRecent')}
              </p>
              {log.map((entry, i) => (
                <LogRow
                  key={entry.id}
                  entry={entry}
                  index={i}
                  onOpen={navigate}
                  onDismiss={dismiss}
                  t={t}
                />
              ))}
            </div>
          )}
        </div>
      </Popover>
    </div>
  );
}
