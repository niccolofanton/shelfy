import React, { useEffect, useRef } from 'react';
import {
  AlertTriangle,
  Award,
  CheckCircle2,
  Clock,
  Eye,
  ExternalLink,
  Image as ImageIcon,
  Info,
  Layers,
  Loader2,
  Palette,
  RotateCw,
  Save,
  ShieldAlert,
  Sparkles,
  X,
} from 'lucide-react';
import { useT, useLang, localeTag } from '../../../i18n';
import type { AiJobView, SiteView, TimelineEvent, WebJob, WebStatus } from '../model';
import { ACTIVE_STATUSES, aiStep, pathOf, streamPreview } from '../model';
import { useVocab } from '../vocab';
import { ACCENT, BlockTitle, Muted } from '../ui';

// Capture: the pipeline behind the reference — live phase stepper and event
// timeline while a job runs (or the anti-bot check to pass), the AI catalog
// state, the archived versions, and the capture facts (engine, discovery,
// per-page timings and QC, skipped pages). Re-capture / re-analyse live here.

export interface VersionEntry {
  id: number | null;
  capturedAt: number | null;
  isCurrent: boolean;
}

const PHASES: WebStatus[] = [
  'pending',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
  'done',
];
function phaseIndex(status: WebStatus): number {
  if (status === 'queued') return 0;
  const i = PHASES.indexOf(status);
  return i;
}
const AI_PHASES = [
  'aiStep.queue',
  'aiStep.extract',
  'aiStep.observe',
  'aiStep.catalog',
  'aiStep.write',
  'aiStep.done',
];

interface CaptureTabProps {
  site: SiteView;
  job: WebJob | null;
  aiJob: AiJobView | null;
  modelReady: boolean;
  now: number;
  versions: VersionEntry[];
  activeVersionId: number | null;
  onSelectVersion: (id: number | null) => void;
  onDeleteSnapshot: (id: number) => void;
  onRecapture: () => void;
  onReanalyse: () => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  onUnblock: (key: string) => void;
  onAiCancel: (key: string) => void;
  onAiRetry: (key: string) => void;
  unblocking: boolean;
}

export default function CaptureTab({
  site,
  job,
  aiJob,
  modelReady,
  now,
  versions,
  activeVersionId,
  onSelectVersion,
  onDeleteSnapshot,
  onRecapture,
  onReanalyse,
  onCancel,
  onRetry,
  onUnblock,
  onAiCancel,
  onAiRetry,
  unblocking,
}: CaptureTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const vocab = useVocab();
  const { lang } = useLang();
  const cap = site.meta.capture;
  const status = job?.status;
  const running = !!status && ACTIVE_STATUSES.has(status);
  const aiRunning = !!aiJob && ['pending', 'extracting', 'analyzing'].includes(aiJob.status);
  const aiIdx = aiJob
    ? aiStep(aiJob)
    : site.ai || site.legacy.description
      ? AI_PHASES.length - 1
      : -1;
  const preview = aiRunning && aiJob ? streamPreview(aiJob.streamText) : null;
  const elapsed =
    job?.startedAt && (job.finishedAt || now) >= job.startedAt
      ? Math.round(((job.finishedAt || now) - job.startedAt) / 1000)
      : null;
  const timings = new Map((cap?.timings || []).map((x) => [x.url, x.ms]));
  const dateFmt = (ts: number | null): string => {
    if (!ts) return '';
    try {
      return new Date(ts).toLocaleString(localeTag(lang), {
        day: '2-digit',
        month: 'short',
        year: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      });
    } catch {
      return '';
    }
  };

  return (
    <div className="flex flex-col gap-9" data-testid="aiweb-tab-capture">
      {/* ── Actions ────────────────────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          data-testid="aiweb-recapture"
          disabled={running}
          onClick={onRecapture}
          className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-[#1f1f1f] border border-[#2e2e2e] text-[12.5px] text-[#e6e6e6] hover:bg-[#262626] u-press disabled:opacity-40"
        >
          <RotateCw size={13} /> {t('recapture')}
        </button>
        <button
          type="button"
          data-testid="aiweb-reanalyse"
          disabled={aiRunning || !modelReady || !site.hasCapture}
          onClick={onReanalyse}
          title={modelReady ? t('reanalyseTitle') : t('aiModelNotReady')}
          className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-[#1f1f1f] border border-[#2e2e2e] text-[12.5px] text-[#e6e6e6] hover:bg-[#262626] u-press disabled:opacity-40"
        >
          <Sparkles size={13} /> {t('reanalyse')}
        </button>
        <span className="ml-auto text-[11.5px] text-[#6f6f6f]">{t('recaptureHint')}</span>
      </div>

      {/* ── Live job ───────────────────────────────────────────────────── */}
      {job && status && status !== 'done' && (
        <section data-testid="aiweb-live">
          <BlockTitle
            icon={running ? Loader2 : status === 'blocked' ? ShieldAlert : AlertTriangle}
            right={
              <>
                {elapsed !== null && (
                  <span className="flex items-center gap-1 text-[11.5px] tabular-nums text-[#6f6f6f]">
                    <Clock size={11} />{' '}
                    {elapsed < 60 ? `${elapsed}s` : `${Math.floor(elapsed / 60)}m ${elapsed % 60}s`}
                  </span>
                )}
                {running && (
                  <button
                    type="button"
                    data-testid="aiweb-detail-cancel"
                    onClick={() => onCancel(job.key)}
                    className="flex items-center gap-1 h-7 px-2 rounded-md text-[12px] text-[#ef5350] hover:bg-[#2a1515] u-press"
                  >
                    <X size={12} /> {tc('cancel')}
                  </button>
                )}
                {(status === 'error' || status === 'cancelled') && (
                  <button
                    type="button"
                    data-testid="aiweb-detail-retry"
                    onClick={() => onRetry(job.key)}
                    className="flex items-center gap-1 h-7 px-2 rounded-md text-[12px] text-[#c9bcff] hover:bg-[#1f1a33] u-press"
                  >
                    <RotateCw size={12} /> {tc('retry')}
                  </button>
                )}
              </>
            }
          >
            {t(`status.${status}`)}
          </BlockTitle>
          {status !== 'blocked' && (
            <Stepper
              labels={PHASES.map((p) => t(`phase.${p}`))}
              active={phaseIndex(status)}
              running={running}
            />
          )}
          {job.stage && <p className="mt-3 text-[12.5px] text-[#a0a0a0]">{job.stage}</p>}
          {status === 'error' && job.error && (
            <p className="mt-3 flex items-center gap-1.5 text-[12.5px] text-[#ef5350]">
              <AlertTriangle size={13} /> {job.error}
            </p>
          )}
          {status === 'blocked' && (
            <div className="mt-2 rounded-xl border border-[#f0b429]/30 bg-[#f0b429]/[0.06] px-4 py-3.5 flex flex-col gap-3">
              <p className="text-[13px] leading-relaxed text-[#e8d6a2]">
                {t('blockedExplain', { vendor: job.blocked?.vendor || t('blockedVendorUnknown') })}
              </p>
              {job.blocked?.url && (
                <p className="text-[11.5px] text-[#a08d5a] truncate">
                  {job.blocked.url}
                  {job.blocked.reason ? ` · ${job.blocked.reason}` : ''}
                </p>
              )}
              <button
                type="button"
                data-testid="aiweb-detail-unblock"
                disabled={unblocking}
                onClick={() => onUnblock(job.key)}
                className="self-start flex items-center gap-1.5 h-9 px-3.5 rounded-lg bg-[#f0b429] text-black text-[13px] font-semibold u-press hover:bg-[#ffc53d] disabled:opacity-70"
              >
                {unblocking ? <Loader2 size={14} className="u-spin" /> : <ExternalLink size={14} />}
                {unblocking ? t('unblockWaiting') : t('unblockAction')}
              </button>
            </div>
          )}
        </section>
      )}

      {/* ── AI catalog ─────────────────────────────────────────────────── */}
      <section data-testid="aiweb-ai">
        <BlockTitle
          icon={Sparkles}
          right={
            <>
              {(aiJob?.model || site.ai?.model || site.legacy.model) && (
                <span className="text-[11.5px] text-[#6f6f6f]">
                  {aiJob?.model || site.ai?.model || site.legacy.model}
                </span>
              )}
              {aiRunning && aiJob && (
                <button
                  type="button"
                  onClick={() => onAiCancel(aiJob.key)}
                  className="flex items-center gap-1 h-7 px-2 rounded-md text-[12px] text-[#ef5350] hover:bg-[#2a1515] u-press"
                >
                  <X size={12} /> {tc('cancel')}
                </button>
              )}
              {aiJob?.status === 'error' && (
                <button
                  type="button"
                  onClick={() => onAiRetry(aiJob.key)}
                  className="flex items-center gap-1 h-7 px-2 rounded-md text-[12px] text-[#c9bcff] hover:bg-[#1f1a33] u-press"
                >
                  <RotateCw size={12} /> {tc('retry')}
                </button>
              )}
            </>
          }
        >
          {t('aiTitle')}
        </BlockTitle>
        <Stepper labels={AI_PHASES.map((k) => t(k))} active={aiIdx} running={aiRunning} />
        {aiJob?.status === 'error' && aiJob.error && (
          <p className="mt-3 flex items-center gap-1.5 text-[12.5px] text-[#ef5350]">
            <AlertTriangle size={13} /> {aiJob.error}
          </p>
        )}
        {preview?.text && (
          <p
            className="mt-3 text-[12.5px] leading-relaxed text-[#b7b0d8]"
            data-testid="aiweb-ai-stream-capture"
          >
            <span className="text-[#8b74ff]">{t(`streamKey.${preview.key}`)} · </span>
            {preview.text}
            <span className="opacity-60">▋</span>
          </p>
        )}
        {!aiJob && !site.ai && !site.legacy.description && (
          <p className="mt-3 text-[12.5px] text-[#6f6f6f]">
            {modelReady ? t('aiMissing') : t('aiModelNotReady')}
          </p>
        )}
        {!aiJob && site.ai && site.ai.schema < 2 && (
          <p className="mt-3 text-[12.5px] text-[#f0b429]">{t('aiOutdated')}</p>
        )}
      </section>

      {/* ── Timeline (live job only — the event log is ephemeral) ──────── */}
      {job && job.events.length > 0 && (
        <section>
          <BlockTitle icon={Layers} count={job.events.length}>
            {t('behindTheScenes')}
          </BlockTitle>
          <Timeline events={job.events} active={running} />
        </section>
      )}

      {/* ── Versions ───────────────────────────────────────────────────── */}
      {versions.length > 1 && (
        <section data-testid="aiweb-versions">
          <BlockTitle icon={Layers} count={versions.length}>
            {t('versions')}
          </BlockTitle>
          <div className="flex flex-wrap gap-1.5">
            {versions.map((v) => {
              const active = v.id === activeVersionId;
              return (
                <div key={v.id ?? 'current'} className="group relative">
                  <button
                    type="button"
                    data-testid="aiweb-version-chip"
                    onClick={() => onSelectVersion(v.id)}
                    title={v.isCurrent ? t('versionCurrentTitle') : t('versionArchivedTitle')}
                    className={`flex items-center gap-1.5 h-7 px-2.5 rounded-full text-[11.5px] u-press border ${
                      active
                        ? 'bg-[#7B5CFF] border-[#7B5CFF] text-white'
                        : 'bg-[#1a1a1a] border-[#2e2e2e] text-[#bdbdbd] hover:text-white'
                    }`}
                  >
                    {v.isCurrent && (
                      <span
                        className="w-1.5 h-1.5 rounded-full"
                        style={{ background: active ? '#fff' : '#4caf50' }}
                      />
                    )}
                    <span className="tabular-nums">
                      {v.isCurrent ? t('versionCurrent') : dateFmt(v.capturedAt)}
                    </span>
                  </button>
                  {!v.isCurrent && v.id !== null && (
                    <button
                      type="button"
                      data-testid="aiweb-version-delete"
                      onClick={() => onDeleteSnapshot(v.id as number)}
                      title={t('versionDeleteTitle')}
                      aria-label={t('versionDeleteTitle')}
                      className="absolute -top-1.5 -right-1.5 hidden group-hover:flex items-center justify-center w-4 h-4 rounded-full bg-[#ef5350] text-white u-press"
                    >
                      <X size={9} />
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}

      {/* ── Capture facts ──────────────────────────────────────────────── */}
      <section>
        <BlockTitle icon={Info}>{t('captureFacts')}</BlockTitle>
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-[12.5px] max-w-2xl">
          {site.capturedAt && <FactRow label={t('capturedAt')}>{dateFmt(site.capturedAt)}</FactRow>}
          {cap?.engine && <FactRow label={t('captureEngine')}>{cap.engine}</FactRow>}
          {cap?.discovery && (
            <FactRow label={t('captureDiscovery')}>
              {(() => {
                const label = t(`discovery.${cap.discovery}`);
                return label.startsWith('aiWebsites.') ? cap.discovery : label;
              })()}
            </FactRow>
          )}
          {cap?.viewport && <FactRow label={t('captureViewport')}>{cap.viewport}</FactRow>}
          {cap?.consent && <FactRow label={t('captureConsent')}>{cap.consent}</FactRow>}
          <FactRow label={t('captureFormat')}>{site.isV2 ? t('formatV2') : t('formatV1')}</FactRow>
        </dl>
        {!cap && !site.capturedAt && <Muted>{t('captureFactsEmpty')}</Muted>}
      </section>

      {site.pages.length > 0 && (
        <section>
          <BlockTitle icon={ImageIcon} count={site.pages.length}>
            {t('capturedPages')}
          </BlockTitle>
          <table className="w-full max-w-3xl text-[12.5px]">
            <tbody>
              {site.pages.map((p, i) => {
                const ms = timings.get(p.url);
                const ok = !p.qcStatus || p.qcStatus === 'ok';
                return (
                  <tr key={`${p.url}-${i}`} className="border-t border-[#202020]">
                    <td className="py-2 pr-4 text-[#bdbdbd] whitespace-nowrap">
                      {p.pageType ? vocab.label('pageType', p.pageType) : '—'}
                    </td>
                    <td className="py-2 pr-4 text-[#8a8a8a] max-w-[320px] truncate">
                      {pathOf(p.url)}
                    </td>
                    <td className="py-2 pr-4 whitespace-nowrap">
                      {ok ? (
                        <span className="inline-flex items-center gap-1 text-[#4caf50]">
                          <CheckCircle2 size={12} /> {vocab.label('qc', 'ok')}
                        </span>
                      ) : (
                        <span
                          className="inline-flex items-center gap-1 text-[#f0b429]"
                          title={p.qcReason}
                        >
                          <AlertTriangle size={12} /> {vocab.label('qc', p.qcStatus)}
                        </span>
                      )}
                    </td>
                    <td className="py-2 text-right tabular-nums text-[#6f6f6f] whitespace-nowrap">
                      {ms ? `${(ms / 1000).toFixed(1)}s` : ''}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>
      )}

      {cap && cap.skipped.length > 0 && (
        <section data-testid="aiweb-skipped">
          <BlockTitle icon={AlertTriangle} count={cap.skipped.length}>
            {t('skippedPages')}
          </BlockTitle>
          <ul className="flex flex-col gap-1 text-[12.5px]">
            {cap.skipped.map((s, i) => (
              <li key={`${s.url}-${i}`} className="flex gap-3">
                <span className="text-[#bdbdbd] truncate max-w-[360px]">{pathOf(s.url)}</span>
                <span className="text-[#6f6f6f] truncate">{s.reason}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

function FactRow({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}): React.ReactElement {
  return (
    <>
      <dt className="text-[#6f6f6f] whitespace-nowrap">{label}</dt>
      <dd className="text-[#dcdcdc] min-w-0 truncate">{children}</dd>
    </>
  );
}

// ── Segmented stepper (app accent) ─────────────────────────────────────────
function Stepper({
  labels,
  active,
  running,
}: {
  labels: string[];
  active: number;
  running: boolean;
}): React.ReactElement {
  const failed = active < 0;
  return (
    <div className="flex items-stretch gap-1.5" data-testid="aiweb-stepper">
      {labels.map((label, i) => {
        const current = i === active && running && !failed;
        const done = !failed && i <= active && !current;
        return (
          <div key={label} className="flex-1 min-w-0 flex flex-col gap-1.5">
            <div
              className="w-full h-1 rounded-full overflow-hidden u-transition"
              style={{ background: done ? ACCENT : current ? `${ACCENT}33` : '#262626' }}
            >
              {current && <div className="ai-progress-track w-full h-full" />}
            </div>
            <span
              className={`truncate text-[10.5px] ${current ? 'aiweb-step-current font-semibold' : 'font-medium'}`}
              style={{ color: current ? ACCENT : done ? '#a0a0a0' : '#5a5a5a' }}
            >
              {label}
            </span>
          </div>
        );
      })}
    </div>
  );
}

// ── Live event timeline ─────────────────────────────────────────────────────
function EventIcon({ kind }: { kind: string }): React.ReactElement {
  const p = { size: 13, className: 'shrink-0' };
  switch (kind) {
    case 'read':
      return <Eye {...p} style={{ color: '#60a5fa' }} />;
    case 'artifact':
      return <ImageIcon {...p} style={{ color: '#a78bfa' }} />;
    case 'branding':
      return <Palette {...p} style={{ color: '#f0b429' }} />;
    case 'awards':
      return <Award {...p} style={{ color: '#f0b429' }} />;
    case 'write':
      return <Save {...p} style={{ color: '#4caf50' }} />;
    case 'error':
      return <AlertTriangle {...p} style={{ color: '#ef5350' }} />;
    default:
      return <Info {...p} style={{ color: '#6b6b6b' }} />;
  }
}

function Timeline({
  events,
  active,
}: {
  events: TimelineEvent[];
  active: boolean;
}): React.ReactElement {
  const { lang } = useLang();
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (active && ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [events, active]);
  const clock = (ts: number): string => {
    if (!ts) return '';
    try {
      return new Date(ts).toLocaleTimeString(localeTag(lang), {
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      });
    } catch {
      return '';
    }
  };
  return (
    <div
      ref={ref}
      data-testid="aiweb-timeline"
      className="max-h-[420px] overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] rounded-xl border border-[#232323] bg-[#131313] px-3 py-2"
    >
      {events.map((e) => (
        <div key={e.id} className="flex items-start gap-2.5 py-1.5">
          <span className="pt-[2px]">
            <EventIcon kind={e.kind} />
          </span>
          <div className="flex-1 min-w-0">
            <div
              className={`text-[12px] leading-snug ${e.kind === 'error' ? 'text-[#ef5350]' : 'text-[#b8b8b8]'}`}
            >
              {e.text}
            </div>
            {e.pages.length > 0 && (
              <ul className="mt-1 flex flex-col gap-0.5">
                {e.pages.map((p, i) => (
                  <li key={i} className="text-[11px] text-[#6b6b6b] truncate">
                    {pathOf(p)}
                  </li>
                ))}
              </ul>
            )}
          </div>
          <span className="pt-[2px] text-[10.5px] tabular-nums text-[#5a5a5a] shrink-0">
            {clock(e.ts)}
          </span>
        </div>
      ))}
    </div>
  );
}
