import React, { useEffect, useMemo, useRef, useState } from 'react';
import {
  Sparkles,
  Cpu,
  MemoryStick,
  Zap,
  HardDrive,
  Mic,
  ScanSearch,
  Check,
  ChevronDown,
  Download,
  Loader2,
  AlertTriangle,
  Star,
  Boxes,
  ShieldCheck,
} from 'lucide-react';
import { useT, useLang, localeTag } from '../i18n';
import { fetchAiSetup } from '../hooks/useAiSetup';

// ── First-run AI onboarding ─────────────────────────────────────────────────────
// Covers the AI tabs (via App's gate) until the local pipeline is fully ready.
// One linear flow: detected hardware → one decision (the analysis model, with the
// hardware-fit preset preselected) → a transparent install checklist (voice model,
// engine, tag clustering) → a single CTA that downloads everything sequentially,
// top to bottom, with live per-item progress. Everything runs through the same
// IPC surface as Settings, so the two stay consistent and downloads keep going in
// the background if the user navigates away (the gate overlay stays mounted).

// Acceleration backends are brand names — shown verbatim, no i18n.
const ACCEL_LABELS = { metal: 'Apple Metal', cuda: 'NVIDIA CUDA', vulkan: 'Vulkan', cpu: 'CPU' };

// Stagger for the entrance animations (hero → hardware → steps → CTA).
const delay = (i) => ({ animationDelay: `${i * 60}ms` });

function fmtGB(n, lang) {
  if (n == null) return null;
  if (n < 1) return `${Math.round(n * 1000)} MB`;
  return `${Number(n).toLocaleString(localeTag(lang), { maximumFractionDigits: 1 })} GB`;
}

// ── Small chrome ────────────────────────────────────────────────────────────────

function HwChip({ icon: Icon, label, value }) {
  return (
    <div className="flex items-center gap-2 rounded-lg bg-[#1c1c1c] border border-[#2a2a2a] px-3 py-2 min-w-0">
      <Icon size={14} className="text-gray-500 shrink-0" />
      <span className="text-gray-500 text-[11px] shrink-0">{label}</span>
      <span className="text-gray-200 text-[11px] font-medium ml-auto truncate">{value}</span>
    </div>
  );
}

// Right-hand status of an install item: size → progress → check.
function ItemStatus({ state, sizeText }) {
  const t = useT('aiOnboarding');
  if (state.status === 'done') {
    return (
      <span className="u-pop-in flex items-center gap-1 text-[11px] font-medium text-emerald-400 shrink-0">
        <Check size={13} /> {t('statusInstalled')}
      </span>
    );
  }
  if (state.status === 'run') {
    const pct = Math.round((state.progress || 0) * 100);
    return (
      <span className="flex items-center gap-1.5 text-[11px] font-medium tabular-nums shrink-0 text-[var(--accent)]">
        <Loader2 size={13} className="u-spin" />
        {state.phase === 'extract' ? t('statusExtracting') : t('statusDownloading', { pct })}
      </span>
    );
  }
  if (state.status === 'error') {
    return (
      <span className="flex items-center gap-1 text-[11px] font-medium text-red-400 shrink-0">
        <AlertTriangle size={13} /> {t('statusError')}
      </span>
    );
  }
  if (state.status === 'wait') {
    return <span className="text-[11px] text-gray-500 shrink-0">{t('statusWaiting')}</span>;
  }
  return (
    <span className="flex items-center gap-1.5 text-[11px] text-gray-500 shrink-0">
      {sizeText && <span className="tabular-nums">{sizeText}</span>}
      <Download size={12} />
    </span>
  );
}

// One numbered step card. The vertical connector + number circle give the flow
// its linear, guided read; the body hosts the item description (and, for the
// model step, the selection UI).
function StepCard({ index, last, icon: Icon, title, desc, state, sizeText, error, children }) {
  const active = state.status === 'run';
  return (
    <div className="relative flex gap-4" data-testid={`ai-onb-item-${index}`}>
      {/* Rail: number circle + connector to the next step */}
      <div className="flex flex-col items-center shrink-0">
        <div
          className="flex items-center justify-center w-7 h-7 rounded-full border text-[11px] font-semibold u-transition"
          style={
            state.status === 'done'
              ? {
                  borderColor: 'transparent',
                  background: 'rgba(52,211,153,0.15)',
                  color: '#34d399',
                }
              : active
                ? {
                    borderColor: 'var(--accent)',
                    background: 'rgba(123,92,255,0.12)',
                    color: 'var(--accent)',
                  }
                : { borderColor: '#2e2e2e', background: '#161616', color: '#9ca3af' }
          }
        >
          {state.status === 'done' ? <Check size={13} /> : index}
        </div>
        {!last && <div className="w-px flex-1 my-1.5" style={{ background: '#242424' }} />}
      </div>

      {/* Body */}
      <div
        className="flex-1 min-w-0 rounded-xl border p-4 mb-3 u-transition"
        style={{
          borderColor: active ? 'rgba(123,92,255,0.45)' : '#242424',
          background: active ? 'rgba(123,92,255,0.04)' : '#161616',
        }}
      >
        <div className="flex items-start gap-3">
          <Icon size={16} className="text-gray-400 mt-0.5 shrink-0" />
          <div className="flex-1 min-w-0">
            <p className="text-white text-sm font-medium leading-tight">{title}</p>
            <p className="text-gray-500 text-xs mt-1">{desc}</p>
          </div>
          <ItemStatus state={state} sizeText={sizeText} />
        </div>

        {children}

        {state.status === 'run' && (
          <div className="u-fade-in h-1 mt-3 rounded bg-[#2a2a2a] overflow-hidden">
            <div
              className="u-progress h-full bg-[var(--accent)]"
              style={{ width: `${Math.round((state.progress || 0) * 100)}%` }}
            />
          </div>
        )}
        {state.status === 'error' && error && (
          <p className="u-fade-in text-red-400 text-[11px] mt-2 break-words">{error}</p>
        )}
      </div>
    </div>
  );
}

// Selectable model row (the recommended pick + the expandable alternatives).
function ModelOption({ model, selected, recommended, ramShortfall, disabled, onSelect }) {
  const t = useT('aiOnboarding');
  const { lang } = useLang();
  return (
    <button
      type="button"
      data-testid={`ai-onb-model-${model.id}`}
      aria-pressed={selected}
      disabled={disabled}
      onClick={() => onSelect(model.id)}
      className={`u-press w-full text-left rounded-lg border p-3 u-transition disabled:opacity-50 ${
        selected
          ? 'border-[var(--accent)] bg-[var(--accent)]/5'
          : 'border-[#2a2a2a] bg-[#191919] hover:bg-[#1e1e1e]'
      }`}
    >
      <div className="flex items-center gap-2.5">
        <span
          className={`w-3.5 h-3.5 rounded-full border-2 shrink-0 u-transition ${
            selected ? 'border-[var(--accent)] bg-[var(--accent)]' : 'border-gray-600'
          }`}
        />
        <span className="text-white text-sm font-medium">{model.name}</span>
        <span className="text-[10px] uppercase tracking-wider text-gray-400 bg-[#2a2a2a] rounded px-1.5 py-0.5">
          {model.tier}
        </span>
        {recommended && (
          <span className="flex items-center gap-1 text-[10px] text-amber-400 ml-auto shrink-0">
            <Star size={11} className="fill-amber-400" /> {t('recommendedForYou')}
          </span>
        )}
      </div>
      <p className="text-gray-500 text-xs mt-1.5 ml-6">{model.note}</p>
      <div className="flex items-center gap-3 mt-1.5 ml-6 text-[11px] text-gray-500">
        <span className="flex items-center gap-1">
          <HardDrive size={11} /> {fmtGB(model.sizeGB, lang)}
        </span>
        {model.minRamGB != null && (
          <span className="flex items-center gap-1">
            <Cpu size={11} /> {t('ramRequirement', { n: model.minRamGB })}
          </span>
        )}
        {ramShortfall && (
          <span className="flex items-center gap-1 text-amber-400">
            <AlertTriangle size={11} /> {t('ramWarning')}
          </span>
        )}
      </div>
    </button>
  );
}

// ── Main view ───────────────────────────────────────────────────────────────────

export default function AiOnboarding({ onDone, onSkip, onOpenSettings }) {
  const t = useT('aiOnboarding');
  const { lang } = useLang();

  const [setup, setSetup] = useState(null); // { binaries, vlm, stt, emb, complete }
  const [hwInfo, setHwInfo] = useState(null); // { hardware, recommendedModelId, ... }
  const [variant, setVariant] = useState(null); // { effective, recommended, ... }
  const [selectedVlm, setSelectedVlm] = useState(null);
  const [showAlts, setShowAlts] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [finished, setFinished] = useState(false);
  // Per-family live install state, layered over the on-disk catalogs:
  // { vlm|stt|engine|emb: { status: 'wait'|'run'|'done'|'error', progress, phase, error } }
  const [live, setLive] = useState({});
  const installingRef = useRef(false); // synchronous double-click guard

  const refreshAll = async () => {
    const api = window.electronAPI;
    const safe = (p) => Promise.resolve(p).catch(() => null);
    const [s, h, v] = await Promise.all([
      fetchAiSetup(),
      safe(api?.getHardwareInfo?.()),
      safe(api?.getVariantState?.()),
    ]);
    if (s) setSetup(s);
    if (h) setHwInfo(h);
    if (v) setVariant(v);
    return s;
  };

  useEffect(() => {
    refreshAll();
    // Live progress from main, one listener per family. Each updates its item
    // only — including downloads started elsewhere (e.g. from Settings).
    const api = window.electronAPI;
    // Update an item only when it's untracked (external download) or already
    // running — never promote a 'wait'/'done'/'error' entry owned by the chain.
    const track = (key) => (p) =>
      setLive((s) => {
        const cur = s[key];
        if (cur && cur.status !== 'run') return s;
        return { ...s, [key]: { ...cur, status: 'run', progress: p?.progress ?? 0 } };
      });
    const offs = [
      api?.onModelProgress?.(track('vlm')),
      api?.onSttModelProgress?.(track('stt')),
      api?.onEmbModelProgress?.(track('emb')),
      api?.onBinariesProgress?.((p) =>
        setLive((s) => {
          const prev = s.engine;
          if (prev && prev.status !== 'run') return s;
          // Resolve terminal phases ourselves: Settings' binaries download
          // dispatches no window event, so no onChanged refresh would ever
          // flip a still-'run' engine off — leaving externalBusy stuck true.
          if (p?.phase === 'done') return { ...s, engine: { status: 'done', progress: 1 } };
          if (p?.phase === 'error')
            return { ...s, engine: { ...prev, status: 'error', error: p?.error } };
          return {
            ...s,
            engine: { ...prev, status: 'run', progress: p?.fraction ?? 0, phase: p?.phase },
          };
        }),
      ),
    ];
    return () => offs.forEach((off) => typeof off === 'function' && off());
  }, []);

  // Stay in sync with installs driven elsewhere (the Settings pickers dispatch
  // these on every download/switch). While our own chain runs it owns the state.
  useEffect(() => {
    const onChanged = async () => {
      if (installingRef.current) return;
      await refreshAll();
      setLive((s) => Object.fromEntries(Object.entries(s).filter(([, v]) => v.status === 'error')));
    };
    const evs = ['ai-model-changed', 'stt-model-changed', 'emb-model-changed', 'ai-setup-changed'];
    evs.forEach((e) => window.addEventListener(e, onChanged));
    return () => evs.forEach((e) => window.removeEventListener(e, onChanged));
  }, []);

  // However the pipeline got completed (our chain or Settings), celebrate once.
  useEffect(() => {
    if (setup?.complete) setFinished(true);
  }, [setup?.complete]);

  const hw = hwInfo?.hardware || null;
  const vlmModels = useMemo(() => setup?.vlm || [], [setup]);
  const sttModels = setup?.stt || [];
  const embModels = setup?.emb || [];

  // The hardware-fit recommendation, falling back to the catalog's static flag.
  const recommendedId = useMemo(() => {
    if (hwInfo?.recommendedModelId && vlmModels.some((m) => m.id === hwInfo.recommendedModelId))
      return hwInfo.recommendedModelId;
    return vlmModels.find((m) => m.recommended)?.id || vlmModels[0]?.id || null;
  }, [hwInfo, vlmModels]);

  // Default selection: an already-downloaded model wins, else the recommendation.
  useEffect(() => {
    if (selectedVlm || !vlmModels.length) return;
    const ready = vlmModels.find((m) => m.ready);
    setSelectedVlm(ready?.id || recommendedId);
  }, [vlmModels, recommendedId, selectedVlm]);

  const selModel = vlmModels.find((m) => m.id === selectedVlm) || null;
  const vlmReady = vlmModels.some((m) => m.ready);
  const sttTarget =
    sttModels.find((m) => m.ready) || sttModels.find((m) => m.recommended) || sttModels[0] || null;
  const sttReady = sttModels.some((m) => m.ready);
  const embTarget = embModels[0] || null;
  const embReady = embModels.some((m) => m.ready);
  const engineReady = !!setup?.binaries?.ready;
  const accelLabel =
    ACCEL_LABELS[variant?.effective || variant?.recommended || hw?.recommendedVariant] ||
    ACCEL_LABELS.cpu;

  // Render state per item: the live (chain/event) layer wins, then the catalogs.
  const itemState = (key, ready, downloading) => {
    if (live[key]) return live[key];
    if (downloading) return { status: 'run', progress: 0 };
    if (ready) return { status: 'done' };
    return { status: 'idle' };
  };
  const states = {
    vlm: itemState(
      'vlm',
      vlmReady,
      vlmModels.some((m) => m.downloading),
    ),
    stt: itemState(
      'stt',
      sttReady,
      sttModels.some((m) => m.downloading),
    ),
    engine: itemState('engine', engineReady, false),
    emb: itemState(
      'emb',
      embReady,
      embModels.some((m) => m.downloading),
    ),
  };
  const anyError = Object.values(states).some((s) => s.status === 'error');
  const externalBusy = !installing && Object.values(states).some((s) => s.status === 'run');

  // Missing download volume for the CTA label (engine size is unknown → "≈").
  const totalMissingGB = useMemo(() => {
    let sum = 0;
    if (!vlmReady && selModel?.sizeGB) sum += selModel.sizeGB;
    if (!sttReady && sttTarget?.sizeGB) sum += sttTarget.sizeGB;
    if (!embReady && embTarget?.sizeGB) sum += embTarget.sizeGB;
    return sum;
  }, [vlmReady, sttReady, embReady, selModel, sttTarget, embTarget]);

  // ── Install chain: top to bottom, one item at a time ──────────────────────────
  async function runInstall() {
    if (installingRef.current || !setup) return;
    installingRef.current = true;
    setInstalling(true);
    const api = window.electronAPI;
    const plan = [
      !vlmReady && selectedVlm
        ? { key: 'vlm', run: () => api.downloadModel(selectedVlm), event: 'ai-model-changed' }
        : null,
      !sttReady && sttTarget
        ? { key: 'stt', run: () => api.sttDownloadModel(sttTarget.id), event: 'stt-model-changed' }
        : null,
      !engineReady
        ? {
            key: 'engine',
            // Unlike the model downloads, binaries:ensure resolves with
            // { ok: false, error } instead of rejecting — surface that as a
            // throw so the chain's catch marks the step as errored (Retry).
            run: async () => {
              const r = await api.ensureBinaries(false);
              if (r && r.ok === false) throw new Error(r.error || 'engine-install-failed');
              return r;
            },
            event: 'ai-setup-changed',
          }
        : null,
      !embReady && embTarget
        ? { key: 'emb', run: () => api.embDownloadModel(embTarget.id), event: 'emb-model-changed' }
        : null,
    ].filter(Boolean);

    // Everything queued up front, so the user sees the whole plan immediately.
    setLive(Object.fromEntries(plan.map((p) => [p.key, { status: 'wait', progress: 0 }])));

    try {
      for (const step of plan) {
        setLive((s) => ({ ...s, [step.key]: { status: 'run', progress: 0 } }));
        try {
          await step.run();
        } catch (err) {
          setLive((s) => ({
            ...s,
            [step.key]: { status: 'error', error: String(err?.message || err) },
          }));
          return;
        }
        setLive((s) => ({ ...s, [step.key]: { status: 'done', progress: 1 } }));
        window.dispatchEvent(new Event(step.event));
      }
      const fresh = await refreshAll();
      // Catalogs are now authoritative; keep only error markers (none here).
      setLive({});
      if (fresh?.complete) setFinished(true);
    } finally {
      installingRef.current = false;
      setInstalling(false);
      window.dispatchEvent(new Event('ai-setup-changed'));
    }
  }

  const retryInstall = () => {
    setLive((s) => Object.fromEntries(Object.entries(s).filter(([, v]) => v.status === 'done')));
    runInstall();
  };

  async function startAnalyzing() {
    try {
      await window.electronAPI.analyzeMissing?.();
    } catch {
      /* not fatal — the queue tab explains the state */
    }
    onDone?.();
  }

  // ── Loading skeleton ──────────────────────────────────────────────────────────
  if (!setup) {
    return (
      <div
        data-testid="ai-onboarding"
        className="h-full flex items-center justify-center"
        style={{ background: 'var(--bg-primary)' }}
      >
        <Loader2 size={22} className="u-spin text-gray-600" />
      </div>
    );
  }

  // ── Success screen ────────────────────────────────────────────────────────────
  if (finished) {
    return (
      <div
        data-testid="ai-onb-success"
        className="h-full flex items-center justify-center"
        style={{ background: 'var(--bg-primary)' }}
      >
        <div className="u-fade-in-up flex flex-col items-center text-center gap-3 px-6 max-w-[420px]">
          <div
            className="u-pop-in flex items-center justify-center w-16 h-16 rounded-full"
            style={{
              background: 'rgba(52,211,153,0.12)',
              boxShadow: '0 0 48px rgba(52,211,153,0.18)',
            }}
          >
            <Check size={30} style={{ color: 'var(--success)' }} />
          </div>
          <h1 className="font-display text-xl font-semibold text-white mt-1">{t('doneTitle')}</h1>
          <p className="text-sm text-gray-400">{t('doneSubtitle')}</p>
          <button
            data-testid="ai-onb-start"
            onClick={startAnalyzing}
            className="u-press mt-3 flex items-center gap-2 px-5 py-2.5 rounded-lg text-sm font-medium text-white"
            style={{ background: 'var(--accent)' }}
          >
            <Sparkles size={15} /> {t('startAnalyzing')}
          </button>
          <button
            data-testid="ai-onb-later"
            onClick={() => onDone?.()}
            className="u-press text-xs text-gray-500 hover:text-gray-300 transition-colors mt-1"
          >
            {t('later')}
          </button>
        </div>
      </div>
    );
  }

  // ── Wizard ────────────────────────────────────────────────────────────────────
  const alternatives = vlmModels.filter((m) => m.id !== selectedVlm);
  const ctaLabel = anyError
    ? t('retry')
    : installing || externalBusy
      ? t('installing')
      : selModel?.partial
        ? t('resumeInstall')
        : totalMissingGB > 0
          ? t('installWithSize', {
              size: totalMissingGB.toLocaleString(localeTag(lang), { maximumFractionDigits: 1 }),
            })
          : t('install');

  return (
    <div
      data-testid="ai-onboarding"
      className="h-full overflow-y-auto"
      style={{ background: 'var(--bg-primary)' }}
    >
      <div className="max-w-[660px] mx-auto px-6 py-12 flex flex-col">
        {/* ── Hero ── */}
        <header
          className="u-fade-in-up flex flex-col items-center text-center gap-3"
          style={delay(0)}
        >
          <div
            className="flex items-center justify-center w-14 h-14 rounded-2xl"
            style={{
              background: 'rgba(123,92,255,0.12)',
              boxShadow: '0 0 48px rgba(123,92,255,0.22)',
            }}
          >
            <Sparkles size={26} style={{ color: 'var(--accent)' }} />
          </div>
          <h1 className="font-display text-xl font-semibold text-white mt-1">{t('title')}</h1>
          <p className="text-sm text-gray-400 max-w-[460px] leading-relaxed">{t('subtitle')}</p>
          <span className="flex items-center gap-1.5 text-[11px] text-emerald-400/90 bg-emerald-400/10 rounded-full px-3 py-1 mt-1">
            <ShieldCheck size={12} /> {t('privacyNote')}
          </span>
        </header>

        {/* ── Detected hardware ── */}
        <section className="u-fade-in-up mt-10" style={delay(1)}>
          <p className="text-[10px] uppercase tracking-widest text-gray-500 font-medium mb-2.5">
            {t('hwTitle')}
          </p>
          {hw ? (
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-2" data-testid="ai-onb-hw">
              <HwChip
                icon={Cpu}
                label={t('hwCpu')}
                value={t('coresUnit', { n: hw.cpu?.physical ?? '—' })}
              />
              <HwChip
                icon={MemoryStick}
                label={t('hwRam')}
                value={t('gbUnit', { n: Math.round(hw.totalRamGB ?? 0) })}
              />
              <HwChip icon={Zap} label={t('hwGpu')} value={hw.gpu?.name || '—'} />
              <HwChip icon={Boxes} label={t('hwAccel')} value={accelLabel} />
            </div>
          ) : (
            <p className="flex items-center gap-2 text-xs text-gray-500">
              <Loader2 size={13} className="u-spin" /> {t('hwDetecting')}
            </p>
          )}
        </section>

        {/* ── Steps ── */}
        <section className="mt-8 u-fade-in-up" style={delay(2)}>
          {/* 1 · Analysis model (the one decision) */}
          <StepCard
            index={1}
            icon={ScanSearch}
            title={t('stepModelTitle')}
            desc={t('stepModelDesc')}
            state={states.vlm}
            sizeText={fmtGB(selModel?.sizeGB, lang)}
            error={states.vlm.error}
          >
            {vlmReady ? (
              <p className="text-xs text-gray-400 mt-3 ml-7">
                {t('alreadyInstalledModel', { name: vlmModels.find((m) => m.ready)?.name })}
              </p>
            ) : (
              <div className="mt-3 flex flex-col gap-2">
                {selModel && (
                  <ModelOption
                    model={selModel}
                    selected
                    recommended={selModel.id === recommendedId}
                    ramShortfall={hw && selModel.minRamGB > Math.ceil(hw.totalRamGB ?? 0)}
                    disabled={installing || externalBusy}
                    onSelect={() => {}}
                  />
                )}
                <button
                  type="button"
                  data-testid="ai-onb-alts-toggle"
                  onClick={() => setShowAlts((v) => !v)}
                  className="u-press self-start flex items-center gap-1 text-[11px] text-gray-500 hover:text-gray-300 transition-colors px-1 py-0.5"
                >
                  <ChevronDown
                    size={12}
                    className={`transition-transform duration-200 ${showAlts ? 'rotate-180' : ''}`}
                  />
                  {showAlts
                    ? t('hideAlternatives')
                    : t('showAlternatives', { n: alternatives.length })}
                </button>
                {showAlts && (
                  <div className="u-fade-in flex flex-col gap-2">
                    {alternatives.map((m) => (
                      <ModelOption
                        key={m.id}
                        model={m}
                        selected={false}
                        recommended={m.id === recommendedId}
                        ramShortfall={hw && m.minRamGB > Math.ceil(hw.totalRamGB ?? 0)}
                        disabled={installing || externalBusy}
                        onSelect={(id) => setSelectedVlm(id)}
                      />
                    ))}
                  </div>
                )}
              </div>
            )}
          </StepCard>

          {/* 2 · Voice dictation model */}
          <StepCard
            index={2}
            icon={Mic}
            title={t('stepVoiceTitle')}
            desc={t('stepVoiceDesc')}
            state={states.stt}
            sizeText={sttTarget?.sizeLabel || fmtGB(sttTarget?.sizeGB, lang)}
            error={states.stt.error}
          >
            {sttTarget && !sttReady && (
              <p className="text-[11px] text-gray-500 mt-2.5 ml-7">{sttTarget.name}</p>
            )}
          </StepCard>

          {/* 3 · Runtime engine */}
          <StepCard
            index={3}
            icon={HardDrive}
            title={t('stepEngineTitle')}
            desc={t('stepEngineDesc', { accel: accelLabel })}
            state={states.engine}
            sizeText={null}
            error={states.engine.error}
          >
            <p className="text-[11px] text-gray-500 mt-2.5 ml-7">
              llama.cpp · whisper.cpp · FFmpeg
            </p>
          </StepCard>

          {/* 4 · Tag clustering (embeddings) */}
          <StepCard
            index={4}
            last
            icon={Boxes}
            title={t('stepEmbTitle')}
            desc={t('stepEmbDesc')}
            state={states.emb}
            sizeText={embTarget?.sizeLabel || fmtGB(embTarget?.sizeGB, lang)}
            error={states.emb.error}
          />
        </section>

        {/* ── CTA ── */}
        <section className="u-fade-in-up mt-4 flex flex-col items-center gap-2.5" style={delay(3)}>
          <button
            data-testid={anyError ? 'ai-onb-retry' : 'ai-onb-install'}
            onClick={anyError ? retryInstall : runInstall}
            disabled={installing || externalBusy || (!anyError && !selModel && !vlmReady)}
            className="u-press w-full flex items-center justify-center gap-2 px-5 py-3 rounded-xl text-sm font-medium text-white disabled:opacity-50 disabled:cursor-not-allowed"
            style={{ background: 'var(--accent)' }}
            onMouseEnter={(e) => {
              if (!e.currentTarget.disabled)
                e.currentTarget.style.background = 'var(--accent-hover)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.background = 'var(--accent)';
            }}
          >
            {installing || externalBusy ? (
              <Loader2 size={15} className="u-spin" />
            ) : (
              <Download size={15} />
            )}
            {ctaLabel}
          </button>
          <p className="text-[11px] text-gray-500 text-center">
            {installing || externalBusy ? t('backgroundNote') : t('changeLater')}
          </p>
        </section>

        {/* ── Footer escapes ── */}
        <footer
          className="u-fade-in-up mt-8 pt-4 border-t border-[#1e1e1e] flex items-center justify-between"
          style={delay(4)}
        >
          <button
            data-testid="ai-onb-skip"
            onClick={() => onSkip?.()}
            className="u-press text-xs text-gray-500 hover:text-gray-300 transition-colors"
          >
            {t('skip')}
          </button>
          <button
            data-testid="ai-onb-settings"
            onClick={() => onOpenSettings?.()}
            className="u-press text-xs text-gray-500 hover:text-gray-300 transition-colors"
          >
            {t('advancedSettings')}
          </button>
        </footer>
      </div>
    </div>
  );
}
