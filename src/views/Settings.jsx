import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  Trash2,
  AlertTriangle,
  Download,
  Layers,
  Image,
  Film,
  FileJson,
  Upload,
  X,
  CheckCircle,
  Loader,
  Cpu,
  Star,
  HardDrive,
  Check,
  Pause,
  Play,
  Mic,
  RefreshCw,
  Gauge,
  MemoryStick,
  ShieldAlert,
  Scale,
} from 'lucide-react';
import { useDownloadPrefs } from '../hooks/useDownloadPrefs';
import ImportModal from '../components/ImportModal';
import DisclaimerGate from '../components/DisclaimerGate';
import LanguageCard from '../components/LanguageCard';
import { getDisclaimerAcceptance, DISCLAIMER_VERSION } from '../disclaimer';
import { useT, useLang, localeTag } from '../i18n';

const EXPORT_SOURCES = [
  { key: 'instagram', label: 'Instagram', dotColor: 'bg-pink-500' },
  { key: 'twitter', label: 'X / Twitter', dotColor: 'bg-blue-500' },
  { key: 'pinterest', label: 'Pinterest', dotColor: 'bg-red-600' },
];

// Module-level: labels (brand names) stay literal; `labelKey`/`descKey` resolve
// against the `settings` namespace at render time (hooks can't run up here).
const DOWNLOAD_TYPES = [
  {
    key: 'thumbnail',
    labelKey: 'typeThumbnailLabel',
    descKey: 'typeThumbnailDesc',
    Icon: Layers,
    color: '#a78bfa',
  },
  {
    key: 'image',
    labelKey: 'typeImageLabel',
    descKey: 'typeImageDesc',
    Icon: Image,
    color: '#34d399',
  },
  {
    key: 'video',
    labelKey: 'typeVideoLabel',
    descKey: 'typeVideoDesc',
    Icon: Film,
    color: '#60a5fa',
  },
];

function DownloadTypeToggle({ type, checked, onChange }) {
  const t = useT('settings');
  const { labelKey, descKey, Icon, color } = type;
  const label = t(labelKey);
  return (
    <label className="u-press flex items-center gap-3 py-2.5 px-1 -mx-1 rounded-md cursor-pointer select-none hover:bg-[#1c1c1c]">
      <Icon size={16} className="shrink-0" style={{ color }} />
      <div className="flex-1 min-w-0">
        <p className="text-white text-sm font-medium">{label}</p>
        <p className="text-gray-500 text-xs">{t(descKey)}</p>
      </div>
      <input
        type="checkbox"
        aria-label={label}
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-[var(--accent)] w-4 h-4 shrink-0 cursor-pointer"
      />
    </label>
  );
}

// A single destructive action rendered as a row inside the grouped "Zona
// pericolosa" panel. Two-step confirm (button → conferma/annulla) so nothing
// irreversible fires on a single click. Behaviour mirrors the former card.
function DangerRow({ title, desc, buttonLabel, busyLabel, doneLabel, onConfirm }) {
  const tc = useT('common');
  const [confirming, setConfirming] = useState(false);
  const [loading, setLoading] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState(null);

  async function run() {
    setLoading(true);
    setError(null);
    try {
      await onConfirm();
      setDone(true);
      setConfirming(false);
    } catch (err) {
      console.error('[Settings] DangerRow failed:', err);
      setError(err?.message || tc('genericError'));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="py-4">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="text-white text-sm font-medium">{title}</p>
          <p className="text-gray-500 text-xs mt-1 leading-relaxed">{desc}</p>

          {done && (
            <p className="u-fade-in flex items-center gap-1.5 text-emerald-400 text-xs mt-2">
              <Check size={13} className="u-pop-in shrink-0" /> {doneLabel}
            </p>
          )}
          {error && (
            <p className="u-fade-in flex items-center gap-1.5 text-red-400 text-xs mt-2">
              <AlertTriangle size={13} className="shrink-0" /> {error}
            </p>
          )}
        </div>

        <div className="shrink-0">
          {!confirming ? (
            <button
              onClick={() => {
                setDone(false);
                setError(null);
                setConfirming(true);
              }}
              className="u-press inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium border border-red-900/50 text-red-400 hover:bg-red-950/50 hover:text-red-300 hover:border-red-800 cursor-pointer"
            >
              <Trash2 size={13} className="shrink-0" /> {buttonLabel}
            </button>
          ) : (
            <div className="u-fade-in flex items-center gap-2">
              <span className="hidden sm:flex items-center gap-1 text-amber-400 text-[11px] whitespace-nowrap">
                <AlertTriangle size={12} className="shrink-0" /> {tc('irreversible')}
              </span>
              <button
                onClick={run}
                disabled={loading}
                className="u-press inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-red-700 text-white hover:bg-red-600 cursor-pointer disabled:opacity-50"
              >
                {loading && <Loader size={12} className="animate-spin shrink-0" />}
                {loading ? busyLabel : tc('confirm')}
              </button>
              <button
                onClick={() => setConfirming(false)}
                disabled={loading}
                className="u-press px-3 py-1.5 rounded-lg text-xs bg-[#222] text-gray-300 hover:bg-[#2a2a2a] cursor-pointer disabled:opacity-50"
              >
                {tc('cancel')}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function ExportModal({ onClose }) {
  const t = useT('settings');
  const tc = useT('common');
  const [counts, setCounts] = useState({});
  const [selected, setSelected] = useState(() =>
    Object.fromEntries(EXPORT_SOURCES.map((s) => [s.key, true])),
  );
  const [status, setStatus] = useState('idle'); // idle | exporting | done
  const [result, setResult] = useState(null);
  const [error, setError] = useState(null);

  useEffect(() => {
    window.electronAPI.getStats().then((s) => setCounts(s?.byPlatform ?? {}));
  }, []);

  const chosen = EXPORT_SOURCES.filter((s) => selected[s.key]).map((s) => s.key);

  function toggle(key) {
    setSelected((prev) => ({ ...prev, [key]: !prev[key] }));
  }

  async function run() {
    setStatus('exporting');
    setError(null);
    try {
      const res = await window.electronAPI.exportJSON(chosen);
      if (res?.canceled) {
        setStatus('idle');
        return;
      }
      // Il main process NON rigetta su errore di scrittura (disco pieno, permessi,
      // volume read-only): ritorna { canceled: false, error } senza count/filePath.
      // Senza questo branch cadremmo nello schermo 'done' fingendo un export riuscito.
      if (res?.error) {
        setError(res.error || t('exportFailed'));
        setStatus('idle');
        return;
      }
      setResult(res);
      setStatus('done');
    } catch (err) {
      console.error('[Settings] export failed:', err);
      setError(err?.message || t('exportFailed'));
      setStatus('idle');
    }
  }

  const overlayClick = () => {
    if (status === 'exporting') return;
    onClose();
  };

  return (
    <div
      data-testid="export-modal"
      className="u-backdrop-in fixed inset-0 bg-black/60 flex items-center justify-center z-50"
      onClick={overlayClick}
    >
      <div
        className="u-dialog-in bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl p-6 w-full max-w-sm shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {status === 'done' && result?.filePath ? (
          <>
            <div className="flex flex-col items-center gap-4 py-4 mb-5">
              <CheckCircle size={36} className="u-pop-in text-green-500" />
              <p className="text-white text-sm font-medium text-center">
                {t('exportedCount', { count: result.count })}
              </p>
              <code className="text-[#a0a0a0] text-xs break-all text-center">
                {result.filePath}
              </code>
            </div>
            <button
              onClick={onClose}
              className="u-press w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0]"
            >
              {tc('close')}
            </button>
          </>
        ) : (
          <>
            <div className="flex items-center justify-between mb-4">
              <h2 className="text-white text-lg font-semibold">{t('exportTitle')}</h2>
              <button onClick={onClose} className="u-press text-[#888] hover:text-white">
                <X size={18} />
              </button>
            </div>

            <p className="text-[#888] text-sm mb-4 leading-relaxed">{t('exportDescription')}</p>

            {error && (
              <p className="u-fade-in flex items-center gap-1.5 text-red-400 text-xs mb-4">
                <AlertTriangle size={13} className="shrink-0" /> {error}
              </p>
            )}

            <div className="flex flex-col gap-1 mb-5">
              {EXPORT_SOURCES.map(({ key, label, dotColor }) => (
                <label
                  key={key}
                  className="u-press flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-[#222] cursor-pointer select-none"
                >
                  <input
                    type="checkbox"
                    aria-label={label}
                    checked={!!selected[key]}
                    onChange={() => toggle(key)}
                    className="accent-[var(--accent)] w-4 h-4 shrink-0"
                  />
                  <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${dotColor}`} />
                  <span className="flex-1 text-sm text-white">{label}</span>
                  <span className="text-gray-500 text-xs tabular-nums">
                    {(counts[key] ?? 0).toLocaleString()}
                  </span>
                </label>
              ))}
            </div>

            <div className="flex flex-col gap-2">
              <button
                onClick={run}
                disabled={chosen.length === 0 || status === 'exporting'}
                className="u-press flex items-center justify-center gap-2 w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] disabled:opacity-30 disabled:cursor-not-allowed"
              >
                {status === 'exporting' && <Loader size={15} className="animate-spin" />}
                {status === 'exporting' ? t('exporting') : t('export')}
              </button>
              <button
                onClick={onClose}
                disabled={status === 'exporting'}
                className="u-press w-full px-4 py-2 rounded-lg text-[#888] hover:text-white text-sm disabled:opacity-50"
              >
                {tc('cancel')}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function ModelStateBadge({ model, progress }) {
  const t = useT('settings');
  if (model.downloading) {
    return (
      <span className="text-violet-300 text-xs tabular-nums">
        {Math.round((progress || 0) * 100)}%
      </span>
    );
  }
  if (model.active && model.ready) {
    return (
      <span className="flex items-center gap-1 text-green-400 text-xs font-medium">
        <Check size={13} className="u-pop-in" /> {t('active')}
      </span>
    );
  }
  if (model.ready) {
    return <span className="text-gray-500 text-xs">{t('downloaded')}</span>;
  }
  if (model.active) {
    return <span className="text-yellow-400 text-xs font-medium">{t('toDownload')}</span>;
  }
  return null;
}

// Inline two-click delete: trash → confirm (check / cancel). Stops propagation
// so it never triggers the row's select handler.
function DeleteControl({ name, onDelete }) {
  const t = useT('settings');
  const tc = useT('common');
  const [confirm, setConfirm] = useState(false);
  if (confirm) {
    return (
      <span className="u-fade-in flex items-center gap-1" onClick={(e) => e.stopPropagation()}>
        <button
          type="button"
          title={t('confirmDelete')}
          aria-label={t('confirmDelete')}
          onClick={(e) => {
            e.stopPropagation();
            setConfirm(false);
            onDelete();
          }}
          className="u-press p-1 rounded text-red-400 hover:bg-red-900/40 cursor-pointer"
        >
          <Check size={14} />
        </button>
        <button
          type="button"
          title={tc('cancel')}
          aria-label={t('cancelDelete')}
          onClick={(e) => {
            e.stopPropagation();
            setConfirm(false);
          }}
          className="u-press p-1 rounded text-gray-400 hover:bg-[#2a2a2a] cursor-pointer"
        >
          <X size={14} />
        </button>
      </span>
    );
  }
  return (
    <button
      type="button"
      title={t('deleteFromDisk')}
      aria-label={t('deleteNameFromDisk', { name })}
      onClick={(e) => {
        e.stopPropagation();
        setConfirm(true);
      }}
      className="u-press p-1 rounded text-gray-500 hover:text-red-400 hover:bg-red-900/30 cursor-pointer"
    >
      <Trash2 size={14} />
    </button>
  );
}

const COMPACT_BTN =
  'u-press flex items-center gap-1 px-2.5 py-1.5 rounded text-xs cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed';

function ModelRow({
  model,
  progress,
  busy,
  switching,
  onSelect,
  onDownload,
  onPause,
  onCancel,
  onDelete,
}) {
  const t = useT('settings');
  const tc = useT('common');
  // Ready, non-active models can be activated by click — even while ANOTHER
  // model is downloading, so a local model stays usable during a download.
  const selectable = model.ready && !model.active && !switching;
  const sizeText = model.sizeLabel || (model.sizeGB != null ? `${model.sizeGB} GB` : null);
  const stop = (e) => e.stopPropagation();

  return (
    <div
      role="button"
      aria-pressed={model.active}
      tabIndex={selectable ? 0 : -1}
      onClick={() => selectable && onSelect(model.id)}
      onKeyDown={(e) => {
        if (selectable && (e.key === 'Enter' || e.key === ' ')) {
          e.preventDefault();
          onSelect(model.id);
        }
      }}
      className={`u-press w-full h-full text-left rounded-lg border p-4 ${
        model.active
          ? 'border-[var(--accent)] bg-[var(--accent)]/5'
          : 'border-[#2a2a2a] bg-[#161616]'
      } ${selectable ? 'cursor-pointer hover:bg-[#1c1c1c]' : 'cursor-default'}`}
    >
      <div className="flex items-start gap-3">
        <span
          className={`mt-1 w-3.5 h-3.5 rounded-full border-2 shrink-0 transition-colors ${
            model.active ? 'border-[var(--accent)] bg-[var(--accent)]' : 'border-gray-600'
          }`}
        />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-white text-sm font-medium">{model.name}</span>
            <span className="text-[10px] uppercase tracking-wider text-gray-400 bg-[#2a2a2a] rounded px-1.5 py-0.5">
              {model.tier}
            </span>
            {model.recommended && (
              <span className="flex items-center gap-0.5 text-[10px] text-amber-400">
                <Star size={11} className="fill-amber-400" /> {t('recommended')}
              </span>
            )}
          </div>
          <p className="text-gray-500 text-xs mt-1">{model.note}</p>
          <div className="flex items-center gap-3 mt-2 text-gray-500 text-xs">
            {sizeText && (
              <span className="flex items-center gap-1">
                <HardDrive size={12} /> {sizeText}
              </span>
            )}
            {model.minRamGB != null && (
              <span className="flex items-center gap-1">
                <Cpu size={12} /> {t('ramUnit', { n: model.minRamGB })}
              </span>
            )}
          </div>
        </div>

        <div className="shrink-0 flex items-center gap-2">
          <ModelStateBadge model={model} progress={progress} />
          {model.downloading ? (
            <span className="u-fade-in flex items-center gap-1" onClick={stop}>
              <button
                type="button"
                title={tc('pause')}
                aria-label={t('pauseDownload')}
                onClick={onPause}
                className="u-press p-1 rounded text-gray-300 hover:bg-[#2a2a2a] hover:text-white cursor-pointer"
              >
                <Pause size={14} />
              </button>
              <button
                type="button"
                title={tc('cancel')}
                aria-label={t('cancelDownload')}
                onClick={() => onCancel(model.id)}
                className="u-press p-1 rounded text-red-400 hover:bg-red-900/40 cursor-pointer"
              >
                <X size={14} />
              </button>
            </span>
          ) : model.ready ? (
            <DeleteControl name={model.name} onDelete={() => onDelete(model.id)} />
          ) : model.partial ? (
            <span className="u-fade-in flex items-center gap-1.5" onClick={stop}>
              <button
                type="button"
                onClick={() => onDownload(model.id)}
                disabled={busy}
                title={busy ? t('waitDownloadInProgress') : t('resumeDownload')}
                className={`${COMPACT_BTN} bg-[#2a2a2a] text-gray-200 hover:bg-[#333] hover:text-white`}
              >
                <Play size={13} /> {tc('resume')}
              </button>
              <DeleteControl name={model.name} onDelete={() => onDelete(model.id)} />
            </span>
          ) : (
            <span onClick={stop}>
              <button
                type="button"
                onClick={() => onDownload(model.id)}
                disabled={busy}
                title={busy ? t('waitDownloadInProgress') : t('downloadModel')}
                className={`${COMPACT_BTN} bg-[#2a2a2a] text-gray-200 hover:bg-[#333] hover:text-white`}
              >
                <Download size={13} /> {tc('download')}
              </button>
            </span>
          )}
        </div>
      </div>

      {model.downloading && (
        <div className="u-fade-in h-1 mt-3 rounded bg-[#2a2a2a] overflow-hidden">
          <div
            className="u-progress h-full bg-[var(--accent)]"
            style={{ width: `${Math.round((progress || 0) * 100)}%` }}
          />
        </div>
      )}
    </div>
  );
}

// Generic local-model picker (VLM analysis or whisper STT). Downloads run in the
// background: switching the active model among already-downloaded ones stays
// possible while a different model downloads. `api` adapts the electronAPI calls.
function ModelPicker({ icon: Icon, title, description, api }) {
  const t = useT('settings');
  const [models, setModels] = useState([]);
  const [progress, setProgress] = useState(0); // fraction 0..1 of the active download
  const [switching, setSwitching] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const list = await api.list();
      if (Array.isArray(list)) setModels(list);
    } catch {
      /* API unavailable (e.g. outside Electron) */
    }
  }, [api]);

  useEffect(() => {
    refresh();
    let unsub;
    try {
      unsub = api.onProgress?.((p) => setProgress(p?.progress ?? 0));
    } catch {
      /* ignore */
    }
    return () => {
      if (typeof unsub === 'function') unsub();
    };
  }, [api, refresh]);

  const downloadingId = models.find((m) => m.downloading)?.id ?? null;
  const busy = !!downloadingId;

  const select = useCallback(
    async (id) => {
      setSwitching(true);
      try {
        await api.setModel(id);
        await refresh();
        window.dispatchEvent(new Event(api.changedEvent));
      } catch (err) {
        console.error('[ModelPicker] setModel failed:', err);
      } finally {
        setSwitching(false);
      }
    },
    [api, refresh],
  );

  // Start or resume a background download for a specific model id.
  const download = useCallback(
    async (id) => {
      setProgress(0);
      setModels((ms) => ms.map((m) => (m.id === id ? { ...m, downloading: true } : m))); // optimistic
      try {
        await api.download(id);
      } catch (err) {
        console.error('[ModelPicker] download failed:', err);
      } finally {
        await refresh();
        setProgress(0);
        window.dispatchEvent(new Event(api.changedEvent));
      }
    },
    [api, refresh],
  );

  const pause = useCallback(() => {
    api.pause?.();
  }, [api]);

  const cancel = useCallback(
    async (id) => {
      await api.cancel?.(id);
      await refresh();
      window.dispatchEvent(new Event(api.changedEvent));
    },
    [api, refresh],
  );

  const remove = useCallback(
    async (id) => {
      const next = await api.remove(id);
      if (Array.isArray(next)) setModels(next);
      else await refresh();
      window.dispatchEvent(new Event(api.changedEvent));
    },
    [api, refresh],
  );

  if (!models.length) return null;

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3 mb-4">
        <Icon size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div>
          <p className="text-white text-sm font-medium">{title}</p>
          <p className="text-gray-500 text-xs mt-1">{description}</p>
        </div>
      </div>

      <div className="grid grid-cols-1 xl:grid-cols-2 gap-2.5">
        {models.map((m) => (
          <ModelRow
            key={m.id}
            model={m}
            progress={progress}
            busy={busy}
            switching={switching}
            onSelect={select}
            onDownload={download}
            onPause={pause}
            onCancel={cancel}
            onDelete={remove}
          />
        ))}
      </div>

      {busy && (
        <p className="u-fade-in mt-3 text-gray-500 text-xs flex items-center gap-1.5">
          <Loader size={12} className="animate-spin" />
          {t('downloadInProgressNote')}
        </p>
      )}
    </div>
  );
}

// Lets the user pick how many classifications run in parallel (1..max). Higher =
// faster batches but more VRAM (the local server runs with that many inference
// slots and a proportionally larger context). Default 1 = one at a time.
function ConcurrencyPicker() {
  const t = useT('settings');
  const [value, setValue] = useState(1);
  const [max, setMax] = useState(10);
  const [saving, setSaving] = useState(false);
  const valueRef = useRef(1); // latest applied value, for change detection in onChange

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const r = await window.electronAPI.getAnalyzeConcurrency?.();
        if (alive && r) {
          const v = r.value ?? 1;
          setValue(v);
          valueRef.current = v;
          setMax(r.max ?? 10);
        }
      } catch {
        /* API unavailable (e.g. outside Electron) */
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const onChange = useCallback(async (e) => {
    const n = Number(e.target.value);
    setSaving(true);
    try {
      // Main clamps to [1, max]; trust only the applied value (no optimistic
      // setValue that could briefly show a value main will reject).
      const applied = await window.electronAPI.setAnalyzeConcurrency?.(n);
      if (typeof applied !== 'number') return;
      const changed = applied !== valueRef.current;
      valueRef.current = applied;
      setValue(applied);
      // Only let the analysis hook re-read the slot count (queue ETA) when the
      // applied value actually changed — a clamped no-op shouldn't fan out.
      if (changed) window.dispatchEvent(new Event('ai-concurrency-changed'));
    } finally {
      setSaving(false);
    }
  }, []);

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <Layers size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1">
          <p className="text-white text-sm font-medium">{t('concurrencyTitle')}</p>
          <p className="text-gray-500 text-xs mt-1">{t('concurrencyDesc')}</p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {saving && <Loader size={14} className="animate-spin text-gray-500" />}
          <select
            value={value}
            onChange={onChange}
            disabled={saving}
            aria-label={t('concurrencyAria')}
            className="bg-[#1c1c1c] border border-[#333] text-white text-sm rounded-lg px-3 py-2 focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
          >
            {Array.from({ length: max }, (_, i) => i + 1).map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </div>
      </div>
    </div>
  );
}

// Update channel picker. Stable receives only stable releases; Beta opts into
// the test channel (and still receives stable ones). The choice is persisted in
// the main process (userData) and read by the updater on launch.
function UpdateChannelPicker() {
  const t = useT('settings');
  const tc = useT('common');
  const [channel, setChannel] = useState('stable');
  const [version, setVersion] = useState('');
  const [saving, setSaving] = useState(false);
  const [upd, setUpd] = useState(null); // updater state { status, version, manual, progress }
  const [checking, setChecking] = useState(false);
  const checkTimer = useRef(null); // pending "stop spinning" timeout, cleared on unmount

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [ch, v, st] = await Promise.all([
          window.electronAPI.getUpdateChannel?.(),
          window.electronAPI.getAppVersion?.(),
          window.electronAPI.getUpdateState?.(),
        ]);
        if (!alive) return;
        if (ch) setChannel(ch);
        if (v) setVersion(v);
        if (st) setUpd(st);
      } catch {
        /* API unavailable (e.g. outside Electron) */
      }
    })();
    const off = window.electronAPI.onUpdaterState?.((s) => setUpd(s));
    return () => {
      alive = false;
      off?.();
      // Don't let a pending checking-spinner timeout fire after unmount.
      if (checkTimer.current) clearTimeout(checkTimer.current);
    };
  }, []);

  const onChange = useCallback(async (e) => {
    const ch = e.target.value;
    setChannel(ch);
    setSaving(true);
    try {
      const applied = await window.electronAPI.setUpdateChannel?.(ch);
      if (applied) setChannel(applied);
    } finally {
      setSaving(false);
    }
  }, []);

  const onCheck = useCallback(async () => {
    setChecking(true);
    try {
      const s = await window.electronAPI.checkForUpdates?.();
      if (s) setUpd(s);
    } finally {
      if (checkTimer.current) clearTimeout(checkTimer.current);
      checkTimer.current = setTimeout(() => setChecking(false), 800);
    }
  }, []);

  const status = upd?.status || 'idle';
  const pct = upd?.progress != null ? Math.round(upd.progress * 100) : 0;

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <RefreshCw size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <p className="text-white text-sm font-medium">{t('updateChannelTitle')}</p>
          <p className="text-gray-500 text-xs mt-1">
            <span className="font-medium">{t('updateChannelStable')}</span>
            {t('updateChannelDesc1')}
            <span className="font-medium">{t('updateChannelBeta')}</span>
            {t('updateChannelDesc2')}
          </p>
          {version && (
            <p className="text-gray-600 text-xs mt-2">{t('installedVersion', { version })}</p>
          )}
          {status === 'downloading' && (
            <div className="mt-3">
              <div className="h-1.5 rounded-full bg-[#262626] overflow-hidden">
                <div
                  className="u-progress h-full bg-[var(--accent)]"
                  style={{ width: `${pct}%` }}
                />
              </div>
              <p className="text-gray-500 text-[11px] mt-1">
                {t('downloadingVersion', { version: upd?.version || '', pct })}
              </p>
            </div>
          )}
          {status === 'building' && (
            <div className="mt-3">
              <div className="h-1.5 rounded-full bg-[#262626] ai-progress-track" />
              <p className="text-gray-500 text-[11px] mt-1 truncate" title={upd?.log || ''}>
                {upd?.log || t('buildingVersion', { version: upd?.version || '' })}
              </p>
            </div>
          )}
          {status === 'error' && (
            <p className="text-amber-400 text-xs mt-3 break-words">
              {upd?.error || t('updateError')}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {saving && <Loader size={14} className="animate-spin text-gray-500" />}
          <select
            value={channel}
            onChange={onChange}
            disabled={saving}
            aria-label={t('updateChannelAria')}
            className="bg-[#1c1c1c] border border-[#333] text-white text-sm rounded-lg px-3 py-2 focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
          >
            <option value="stable">{t('updateChannelStable')}</option>
            <option value="beta">{t('updateChannelBeta')}</option>
          </select>
        </div>
      </div>

      {/* Update controls: manual check + restart/download when an update is ready */}
      <div className="mt-4 pt-4 border-t border-[#222] flex items-center justify-between gap-3">
        <p className="text-xs text-gray-500 min-w-0 truncate">
          {(status === 'downloaded' || status === 'built') && (
            <span className="text-emerald-400">{t('updateReady', { version: upd?.version })}</span>
          )}
          {status === 'available' && (
            <span className="text-emerald-400">
              {t('updateAvailable', { version: upd?.version })}
            </span>
          )}
          {status === 'manual' && (
            <span className="text-emerald-400">
              {t('updateAvailable', { version: upd?.version })}
            </span>
          )}
          {status === 'downloading' && <span>{t('updateDownloading')}</span>}
          {status === 'building' && <span>{t('updateBuilding')}</span>}
          {status === 'installing' && <span>{t('updateInstalling')}</span>}
          {(status === 'idle' || status === 'error') && <span>{t('updateUpToDate')}</span>}
        </p>
        <div className="flex items-center gap-2 shrink-0">
          {status === 'available' && (
            <button
              onClick={() => window.electronAPI.rebuildUpdate?.()}
              className="inline-flex items-center gap-1.5 text-xs text-black bg-[var(--accent)] rounded-lg px-3 py-2 font-medium hover:opacity-90"
            >
              <RefreshCw size={13} /> {t('updateNow')}
            </button>
          )}
          {(status === 'downloaded' || status === 'built') && (
            <button
              onClick={() => window.electronAPI.quitAndInstallUpdate?.()}
              className="inline-flex items-center gap-1.5 text-xs text-black bg-[var(--accent)] rounded-lg px-3 py-2 font-medium hover:opacity-90"
            >
              <RefreshCw size={13} /> {t('restartAndInstall')}
            </button>
          )}
          {status === 'manual' && (
            <button
              onClick={() => window.electronAPI.openUpdateDownload?.()}
              className="inline-flex items-center gap-1.5 text-xs text-black bg-[var(--accent)] rounded-lg px-3 py-2 font-medium hover:opacity-90"
            >
              <Download size={13} /> {tc('download')}
            </button>
          )}
          <button
            onClick={onCheck}
            disabled={
              checking ||
              status === 'downloading' ||
              status === 'building' ||
              status === 'installing'
            }
            className="inline-flex items-center gap-1.5 text-xs text-white bg-[#1c1c1c] border border-[#333] rounded-lg px-3 py-2 hover:border-[var(--accent)] disabled:opacity-50"
          >
            {checking ? <Loader size={13} className="animate-spin" /> : <RefreshCw size={13} />}
            {t('checkForUpdates')}
          </button>
        </div>
      </div>
    </div>
  );
}

// Runtime sidecar binaries (yt-dlp, ffmpeg, llama-server, whisper). Not bundled
// in the installer: downloaded once into userData. Shows status, lets the user
// (re)download, and pick the llama GPU variant.
// `labelKey` resolves against the `settings` namespace at render (the parenthetical
// hint is translated; brand names stay literal). Hooks can't run at module scope.
const LLAMA_VARIANTS = [
  { value: 'cpu', labelKey: 'llamaVariantCpu' },
  { value: 'cuda', labelKey: 'llamaVariantCuda' },
  { value: 'vulkan', labelKey: 'llamaVariantVulkan' },
  { value: 'metal', labelKey: 'llamaVariantMetal' },
];

function RuntimeBinariesCard() {
  const t = useT('settings');
  const tc = useT('common');
  const [status, setStatus] = useState(null);
  const [vstate, setVstate] = useState(null); // { variant, explicit, failed[], effective, recommended }
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState(null); // { phase, fraction }
  const [fallbackWarn, setFallbackWarn] = useState(null); // variante GPU appena fallita

  const refresh = useCallback(async () => {
    try {
      const [s, vs] = await Promise.all([
        window.electronAPI.getBinariesStatus?.(),
        window.electronAPI.getVariantState?.(),
      ]);
      if (s) setStatus(s);
      if (vs) setVstate(vs);
    } catch {
      /* outside Electron */
    }
  }, []);

  useEffect(() => {
    refresh();
    const offProgress = window.electronAPI.onBinariesProgress?.((p) => {
      setProgress(p);
      if (p.phase === 'done' || p.phase === 'error') {
        setBusy(false);
        refresh();
      }
    });
    // A GPU build failed to start mid-analysis: the main process is already
    // re-provisioning the CPU build (we'll see it via onBinariesProgress).
    const offFallback = window.electronAPI.onVariantFallback?.(({ failedVariant }) => {
      setFallbackWarn(failedVariant);
      setBusy(true);
      refresh();
    });
    return () => {
      offProgress?.();
      offFallback?.();
    };
  }, [refresh]);

  const download = useCallback(
    async (force) => {
      setBusy(true);
      setProgress({ phase: 'download', fraction: 0 });
      try {
        await window.electronAPI.ensureBinaries?.(force);
      } finally {
        await refresh();
        // Mirror the model pickers: notify a mounted onboarding overlay to re-read
        // setup from disk, so setup.complete / binaries.ready recompute and its
        // stale 'engine' overlay clears (and its success screen shows) once the
        // provisioning started here finishes.
        window.dispatchEvent(new Event('ai-setup-changed'));
      }
    },
    [refresh],
  );

  const onVariant = useCallback(
    async (e) => {
      const v = e.target.value;
      setFallbackWarn(null);
      try {
        await window.electronAPI.setLlamaVariant?.(v);
        await refresh();
        // The new variant is a different llama-server build that isn't on disk yet.
        // status().ready still reports the OLD variant's marker as present, so the
        // card would keep showing "Pronti" while the chosen engine is missing.
        // Force a re-provision now (wipes + refetches the variant) using the same
        // busy/progress UI as the Riscarica button.
        await download(true);
      } catch (err) {
        console.error('[RuntimeBinariesCard] setLlamaVariant failed:', err);
      }
    },
    [refresh, download],
  );

  // The dropdown reflects the user's explicit pick, else the effective (post-fallback) variant.
  const variant = vstate
    ? vstate.explicit && vstate.variant
      ? vstate.variant
      : vstate.effective
    : 'cpu';
  const failed = vstate?.failed || [];
  const ready = status?.ready;
  const pct = progress?.fraction != null ? Math.round(progress.fraction * 100) : null;
  const phaseLabel =
    progress?.phase === 'extract'
      ? t('phaseExtract')
      : progress?.phase === 'error'
        ? t('phaseError', { error: progress.error || '' })
        : progress?.phase === 'done'
          ? t('phaseDone')
          : pct != null
            ? t('phaseDownloadingPct', { pct })
            : t('phaseDownloading');

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <HardDrive size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <p className="text-white text-sm font-medium">{t('runtimeTitle')}</p>
          <p className="text-gray-500 text-xs mt-1">{t('runtimeDesc')}</p>
          <p className="text-xs mt-2">
            {status == null ? (
              <span className="text-gray-600">{t('runtimeChecking')}</span>
            ) : ready ? (
              <span className="text-emerald-400">{t('runtimeReady')}</span>
            ) : (
              <span className="text-amber-400">
                {t('runtimeMissing', { missing: status.missing })}
              </span>
            )}
          </p>
          {(fallbackWarn || failed.length > 0) && (
            <p className="u-fade-in text-amber-400 text-[11px] mt-2 flex items-start gap-1.5">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" />
              {fallbackWarn
                ? t('variantFallbackWarn', { variant: fallbackWarn })
                : t('variantFailedWarn', { variants: failed.join(', ') })}
            </p>
          )}
          {busy && (
            <div className="mt-3">
              <div className="h-1.5 rounded-full bg-[#262626] overflow-hidden">
                <div
                  className="u-progress h-full bg-[var(--accent)]"
                  style={{ width: `${pct ?? 10}%` }}
                />
              </div>
              <p className="text-gray-500 text-[11px] mt-1">{phaseLabel}</p>
            </div>
          )}
        </div>
        <div className="flex flex-col items-end gap-2 shrink-0">
          <select
            value={variant}
            onChange={onVariant}
            disabled={busy}
            aria-label={t('runtimeVariantAria')}
            className="bg-[#1c1c1c] border border-[#333] text-white text-xs rounded-lg px-2 py-1.5 focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
          >
            {LLAMA_VARIANTS.map((v) => (
              <option key={v.value} value={v.value}>
                {t(v.labelKey)}
              </option>
            ))}
          </select>
          <button
            onClick={() => download(true)}
            disabled={busy}
            className="inline-flex items-center gap-1.5 text-xs text-white bg-[#1c1c1c] border border-[#333] rounded-lg px-3 py-2 hover:border-[var(--accent)] disabled:opacity-50"
          >
            {busy ? <Loader size={13} className="animate-spin" /> : <Download size={13} />}
            {ready ? t('redownload') : tc('download')}
          </button>
        </div>
      </div>
    </div>
  );
}

const VLM_MODEL_API = {
  list: () => window.electronAPI.listModels(),
  setModel: (id) => window.electronAPI.setModel(id),
  download: (id) => window.electronAPI.downloadModel(id),
  pause: () => window.electronAPI.pauseModelDownload(),
  cancel: (id) => window.electronAPI.cancelModelDownload(id),
  remove: (id) => window.electronAPI.deleteModel(id),
  onProgress: (cb) => window.electronAPI.onModelProgress?.(cb),
  changedEvent: 'ai-model-changed',
};

const STT_MODEL_API = {
  list: () => window.electronAPI.sttListModels(),
  setModel: (id) => window.electronAPI.sttSetModel(id),
  download: (id) => window.electronAPI.sttDownloadModel(id),
  pause: () => window.electronAPI.sttPauseModelDownload(),
  cancel: (id) => window.electronAPI.sttCancelModelDownload(id),
  remove: (id) => window.electronAPI.sttDeleteModel(id),
  onProgress: (cb) => window.electronAPI.onSttModelProgress?.(cb),
  changedEvent: 'stt-model-changed',
};

const EMB_MODEL_API = {
  list: () => window.electronAPI.embListModels(),
  setModel: (id) => window.electronAPI.embSetModel(id),
  download: (id) => window.electronAPI.embDownloadModel(id),
  pause: () => window.electronAPI.embPauseModelDownload(),
  cancel: (id) => window.electronAPI.embCancelModelDownload(id),
  remove: (id) => window.electronAPI.embDeleteModel(id),
  onProgress: (cb) => window.electronAPI.onEmbModelProgress?.(cb),
  changedEvent: 'emb-model-changed',
};

// Maps an installed-variant id to its `settings` translation key (resolved at
// render via t(VARIANT_LABEL_KEYS[id])). 'CPU' etc. stay literal in the message file.
const VARIANT_LABEL_KEYS = {
  cpu: 'variantCpu',
  cuda: 'variantCuda',
  vulkan: 'variantVulkan',
  metal: 'variantMetal',
};

// One labelled dropdown for a single tuning flag. The first option is always
// "Automatico (<valore rilevato>)"; the rest are manual overrides. `value` is the
// stored override ('auto' or a concrete value); `effective` is what's actually used.
function TuningSelect({ label, hint, value, effective, options, onChange, disabled }) {
  const t = useT('settings');
  const isAuto = value === 'auto' || value == null;
  return (
    <div className="flex items-center justify-between gap-3 py-2">
      <div className="min-w-0">
        <p className="text-white text-xs font-medium">{label}</p>
        {hint && <p className="text-gray-500 text-[11px] mt-0.5">{hint}</p>}
      </div>
      <select
        value={isAuto ? 'auto' : String(value)}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        aria-label={label}
        className="bg-[#1c1c1c] border border-[#333] text-white text-xs rounded-lg px-2 py-1.5 shrink-0 focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
      >
        <option value="auto">{t('tuningAuto', { effective })}</option>
        {options.map((o) => (
          <option key={String(o.value)} value={String(o.value)}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function StatChip({ icon: Icon, label, value }) {
  return (
    <div className="flex items-center gap-2 rounded-lg bg-[#1c1c1c] border border-[#2a2a2a] px-2.5 py-1.5">
      <Icon size={13} className="text-gray-500 shrink-0" />
      <span className="text-gray-500 text-[11px]">{label}</span>
      <span className="text-gray-200 text-[11px] font-medium ml-auto truncate">{value}</span>
    </div>
  );
}

// "Prestazioni e hardware": shows the detected host and lets the user override the
// auto-derived spawn flags (everything defaults to Automatico). Conservative by
// design — the recommended variant/model are surfaced as hints, never auto-applied.
function PerformanceCard() {
  const t = useT('settings');
  const [info, setInfo] = useState(null); // { hardware, tuning, recommendedModelId, recommendedVariant }
  const [stt, setStt] = useState(null); // { effective, auto, override }
  const [saving, setSaving] = useState(false);
  // 'auto' = everything detected, controls locked; 'custom' = controls editable.
  // Derived from whether any override is set, then user-toggled. `prevAnyManual`
  // tracks the last-seen override state so we can reconcile if the resolved
  // overrides change underneath (e.g. a future code path resets tuning): when they
  // transition all-auto → any-manual we flip to Custom; manual → all-auto flips to
  // Auto. We don't fight a user toggle that merely matches the current override set.
  const [customMode, setCustomMode] = useState(false);
  const prevAnyManual = useRef(null); // null until first info+stt load

  const refresh = useCallback(async () => {
    try {
      const [i, s] = await Promise.all([
        window.electronAPI.getHardwareInfo?.(),
        window.electronAPI.sttGetTuning?.(),
      ]);
      if (i) setInfo(i);
      if (s) setStt(s);
    } catch {
      /* outside Electron */
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Re-read after a model/concurrency change: both feed the auto defaults (model
  // size, slot count) so the resolved tuning shifts underneath.
  useEffect(() => {
    const onChanged = () => refresh();
    window.addEventListener('ai-model-changed', onChanged);
    window.addEventListener('ai-concurrency-changed', onChanged);
    return () => {
      window.removeEventListener('ai-model-changed', onChanged);
      window.removeEventListener('ai-concurrency-changed', onChanged);
    };
  }, [refresh]);

  const setVlm = useCallback(async (patch) => {
    setSaving(true);
    try {
      const t = await window.electronAPI.setAnalyzeTuning?.(patch);
      if (t) setInfo((prev) => (prev ? { ...prev, tuning: t } : prev));
    } finally {
      setSaving(false);
    }
  }, []);

  const setSttThreads = useCallback(async (v) => {
    setSaving(true);
    try {
      const t = await window.electronAPI.sttSetTuning?.({ threads: v });
      if (t) setStt(t);
    } finally {
      setSaving(false);
    }
  }, []);

  const resetAll = useCallback(async () => {
    await setVlm({
      threads: 'auto',
      threadsBatch: 'auto',
      gpuLayers: 'auto',
      ubatch: 'auto',
      kvCache: 'auto',
    });
    await setSttThreads('auto');
  }, [setVlm, setSttThreads]);

  // Mode reconciliation: on first load derive 'custom' iff any override is set;
  // afterwards only react to an ACTUAL transition of the resolved overrides
  // (all-auto ↔ any-manual), so an external reset of tuning re-syncs the toggle
  // instead of leaving it stale. A no-change refresh (the common case) leaves the
  // user's own toggle untouched.
  useEffect(() => {
    if (!info || !stt) return;
    const ov = info.tuning?.overrides || {};
    const anyManual = !!(
      Object.values(ov).some((v) => v !== 'auto') ||
      (stt.override && stt.override !== 'auto')
    );
    if (prevAnyManual.current === null) {
      setCustomMode(anyManual); // first load
    } else if (anyManual !== prevAnyManual.current) {
      setCustomMode(anyManual); // overrides changed underneath → reconcile
    }
    prevAnyManual.current = anyManual;
  }, [info, stt]);

  // Switching back to Automatico wipes every override so the detected defaults win.
  const selectAuto = useCallback(async () => {
    setCustomMode(false);
    await resetAll();
  }, [resetAll]);

  if (!info) {
    return (
      <div className="rounded-xl border border-[#242424] bg-[#161616] p-5 text-gray-600 text-xs">
        {t('detectingHardware')}
      </div>
    );
  }

  const hw = info.hardware || {};
  const gpu = hw.gpu || {};
  const eff = info.tuning?.effective || {};
  const ov = info.tuning?.overrides || {};
  const installedVariant = info.tuning?.variant;
  const cores = hw.cpu
    ? hw.cpu.perf
      ? `${hw.cpu.physical} (${hw.cpu.perf}P)`
      : `${hw.cpu.physical}`
    : '—';
  const ramText = hw.totalRamGB != null ? `${hw.totalRamGB} GB` : '—';
  const vramText =
    gpu.vramGB != null
      ? gpu.unified
        ? t('sharedVram', { vram: gpu.vramGB })
        : `${gpu.vramGB} GB`
      : '—';
  const maxThreads = hw.cpu?.logical || 16;
  const threadOpts = Array.from({ length: maxThreads }, (_, i) => ({
    value: i + 1,
    label: `${i + 1}`,
  }));
  const cpuOnly = installedVariant === 'cpu';
  const variantMismatch = info.recommendedVariant && info.recommendedVariant !== installedVariant;

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <Gauge size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2 min-w-0">
              <p className="text-white text-sm font-medium">{t('performanceTitle')}</p>
              {saving && <Loader size={13} className="animate-spin text-gray-500" />}
            </div>
            {/* Auto / Custom toggle */}
            <div className="flex items-center rounded-lg bg-[#1c1c1c] border border-[#333] p-0.5 shrink-0">
              <button
                onClick={selectAuto}
                disabled={saving}
                className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors disabled:opacity-50 ${!customMode ? 'bg-[var(--accent)] text-black' : 'text-gray-400 hover:text-white'}`}
              >
                {t('modeAuto')}
              </button>
              <button
                onClick={() => setCustomMode(true)}
                disabled={saving}
                className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors disabled:opacity-50 ${customMode ? 'bg-[var(--accent)] text-black' : 'text-gray-400 hover:text-white'}`}
              >
                {t('modeCustom')}
              </button>
            </div>
          </div>
          <p className="text-gray-500 text-xs mt-1">
            {customMode ? t('performanceDescCustom') : t('performanceDescAuto')}
          </p>

          {/* Hardware rilevato */}
          <div className="grid grid-cols-2 gap-2 mt-3">
            <StatChip icon={Cpu} label="CPU" value={t('coresUnit', { cores })} />
            <StatChip icon={MemoryStick} label="RAM" value={ramText} />
            <StatChip icon={HardDrive} label="GPU" value={gpu.name || '—'} />
            <StatChip icon={MemoryStick} label="VRAM" value={vramText} />
          </div>

          <p className="text-gray-600 text-[11px] mt-2">
            {t('activeEngine')}
            <span className="text-gray-300">
              {VARIANT_LABEL_KEYS[installedVariant]
                ? t(VARIANT_LABEL_KEYS[installedVariant])
                : installedVariant}
            </span>
            {variantMismatch && (
              <span className="text-amber-400">
                {t('recommendedVariantHint', {
                  variant: VARIANT_LABEL_KEYS[info.recommendedVariant]
                    ? t(VARIANT_LABEL_KEYS[info.recommendedVariant])
                    : info.recommendedVariant,
                })}
              </span>
            )}
          </p>

          {info.tuning?.memoryWarning && (
            <p className="text-amber-400 text-[11px] mt-2 flex items-start gap-1.5">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" /> {info.tuning.memoryWarning}
            </p>
          )}

          {/* Override */}
          <div
            className={`u-transition mt-3 border-t border-[#222] divide-y divide-[#222] ${!customMode ? 'opacity-60' : ''}`}
          >
            <TuningSelect
              label={t('gpuOffloadLabel')}
              hint={cpuOnly ? t('gpuOffloadHintCpu') : t('gpuOffloadHint')}
              value={ov.gpuLayers}
              effective={eff.gpuLayers === 'fit' ? t('gpuOffloadAdaptive') : eff.gpuLayers}
              disabled={saving || cpuOnly || !customMode}
              onChange={(v) => setVlm({ gpuLayers: v === 'auto' ? 'auto' : Number(v) })}
              options={[
                { value: 99, label: t('gpuOffloadAllLayers') },
                { value: 0, label: t('gpuOffloadCpuOnly') },
              ]}
            />
            <TuningSelect
              label={t('analysisThreadsLabel')}
              hint={t('analysisThreadsHint')}
              value={ov.threads}
              effective={eff.threads}
              disabled={saving || !customMode}
              onChange={(v) => setVlm({ threads: v === 'auto' ? 'auto' : Number(v) })}
              options={threadOpts}
            />
            <TuningSelect
              label={t('microBatchLabel')}
              hint={t('microBatchHint')}
              value={ov.ubatch}
              effective={eff.ubatch}
              disabled={saving || !customMode}
              onChange={(v) => setVlm({ ubatch: v === 'auto' ? 'auto' : Number(v) })}
              options={[
                { value: 512, label: '512' },
                { value: 1024, label: '1024' },
                { value: 2048, label: '2048' },
              ]}
            />
            <TuningSelect
              label={t('kvCacheLabel')}
              hint={t('kvCacheHint')}
              value={ov.kvCache}
              effective={eff.kvCache}
              disabled={saving || !customMode}
              onChange={(v) => setVlm({ kvCache: v })}
              options={[
                { value: 'f16', label: t('kvCacheF16') },
                { value: 'q8_0', label: t('kvCacheQ8') },
              ]}
            />
            {stt && (
              <TuningSelect
                label={t('transcriptionThreadsLabel')}
                hint={t('transcriptionThreadsHint')}
                value={stt.override}
                effective={stt.effective}
                disabled={saving || !customMode}
                onChange={(v) => setSttThreads(v === 'auto' ? 'auto' : Number(v))}
                options={threadOpts}
              />
            )}
          </div>

          {customMode && (
            <button
              onClick={resetAll}
              disabled={saving}
              className="u-fade-in mt-3 inline-flex items-center gap-1.5 text-xs text-gray-400 hover:text-white disabled:opacity-50"
            >
              <RefreshCw size={12} /> {t('resetToAuto')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function DataActions({ onChanged }) {
  const t = useT('settings');
  const [showExport, setShowExport] = useState(false);
  const [showImport, setShowImport] = useState(false);

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3 mb-4">
        <FileJson size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div>
          <p className="text-white text-sm font-medium">{t('dataTitle')}</p>
          <p className="text-gray-500 text-xs mt-1">{t('dataDesc')}</p>
        </div>
      </div>

      <div className="flex gap-2">
        <button
          data-testid="open-import-btn"
          onClick={() => setShowImport(true)}
          className="u-press flex items-center gap-2 px-3 py-1.5 rounded text-sm bg-[#2a2a2a] text-gray-200 hover:bg-[#333] hover:text-white cursor-pointer"
        >
          <Upload size={14} />
          {t('importJSON')}
        </button>
        <button
          data-testid="open-export-btn"
          onClick={() => setShowExport(true)}
          className="u-press flex items-center gap-2 px-3 py-1.5 rounded text-sm bg-[#2a2a2a] text-gray-200 hover:bg-[#333] hover:text-white cursor-pointer"
        >
          <Download size={14} />
          {t('exportJSON')}
        </button>
      </div>

      {showExport && <ExportModal onClose={() => setShowExport(false)} />}
      {showImport && (
        <ImportModal onClose={() => setShowImport(false)} onImported={() => onChanged?.()} />
      )}
    </div>
  );
}

function SectionTitle({ children }) {
  return (
    <h2 className="text-[11px] font-semibold uppercase tracking-widest text-gray-600 mb-4 font-display">
      {children}
    </h2>
  );
}

// A page section: title + content, separated from the previous one by the
// parent's divider. The vertical padding gives the divider breathing room; the
// first section sits flush under the header.
function SectionBlock({ title, delay, children }) {
  return (
    <section
      className="u-fade-in-up py-10 first:pt-0 last:pb-0"
      style={delay ? { animationDelay: delay } : undefined}
    >
      <SectionTitle>{title}</SectionTitle>
      {children}
    </section>
  );
}

// Small monospace pill in the header showing the installed app version.
function VersionPill() {
  const [version, setVersion] = useState('');
  useEffect(() => {
    let alive = true;
    Promise.resolve(window.electronAPI?.getAppVersion?.())
      .then((v) => {
        if (alive && v) setVersion(v);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  if (!version) return null;
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-[#262626] bg-[#161616] px-3 py-1 text-[11px] text-gray-400 tabular-nums">
      <span className="w-1.5 h-1.5 rounded-full bg-emerald-500/80" />v{version}
    </span>
  );
}

// Legal notice card: shows the recorded disclaimer acceptance and lets the user
// re-read the full text (DisclaimerGate in dismissible 'review' mode).
function LegalCard() {
  const t = useT('settings');
  const { lang } = useLang();
  const [reviewing, setReviewing] = useState(false);
  const accepted = getDisclaimerAcceptance();

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <Scale size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <p className="text-white text-sm font-medium">{t('legalTitle')}</p>
          <p className="text-gray-500 text-xs mt-1 leading-relaxed">
            {t('legalDesc1')}
            <span className="font-mono text-gray-400">DISCLAIMER.md</span>.
          </p>
          <p className="text-gray-600 text-xs mt-2">
            {accepted
              ? t('legalAccepted', {
                  date: new Date(accepted.acceptedAt).toLocaleString(localeTag(lang), {
                    dateStyle: 'medium',
                    timeStyle: 'short',
                  }),
                  version: accepted.version,
                })
              : t('legalNotAccepted', { version: DISCLAIMER_VERSION })}
          </p>
          <button
            onClick={() => setReviewing(true)}
            className="mt-3 text-[#7B5CFF] text-xs font-medium hover:underline u-press"
          >
            {t('legalReview')}
          </button>
        </div>
      </div>
      {reviewing && <DisclaimerGate mode="review" onClose={() => setReviewing(false)} />}
    </div>
  );
}

function AssetTypesCard({ prefs, setType }) {
  const t = useT('settings');
  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3 mb-4">
        <Download size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div>
          <p className="text-white text-sm font-medium">{t('assetTypesTitle')}</p>
          <p className="text-gray-500 text-xs mt-1">{t('assetTypesDesc')}</p>
        </div>
      </div>

      <div className="divide-y divide-[#2a2a2a]">
        {DOWNLOAD_TYPES.map((type) => (
          <DownloadTypeToggle
            key={type.key}
            type={type}
            checked={!!prefs[type.key]}
            onChange={(value) => setType(type.key, value)}
          />
        ))}
      </div>
    </div>
  );
}

export default function Settings({ onDataCleared }) {
  const { prefs, setType } = useDownloadPrefs();
  const t = useT('settings');
  const tc = useT('common');
  const tl = useT('language');
  const tcDeleting = tc('deleting'); // shared "Eliminazione…" busy label for danger rows

  return (
    <div className="flex-1 h-full overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] bg-[#0f0f0f]">
      <div className="max-w-4xl mx-auto px-8 py-8">
        <header className="u-fade-in-up flex items-start justify-between gap-4">
          <div>
            <h1 className="text-white text-2xl font-semibold font-display tracking-tight">
              {t('pageTitle')}
            </h1>
            <p className="text-gray-500 text-sm mt-1">{t('pageSubtitle')}</p>
          </div>
          <div className="shrink-0 pt-1">
            <VersionPill />
          </div>
        </header>

        <div className="mt-2 flex flex-col divide-y divide-[#1f1f1f]">
          <SectionBlock title={tl('section')} delay="20ms">
            <LanguageCard />
          </SectionBlock>

          <SectionBlock title={t('sectionAi')} delay="40ms">
            <div className="flex flex-col gap-4">
              <ModelPicker
                icon={Cpu}
                title={t('vlmTitle')}
                description={t('vlmDesc')}
                api={VLM_MODEL_API}
              />
              <ConcurrencyPicker />
              <ModelPicker
                icon={Mic}
                title={t('sttTitle')}
                description={t('sttDesc')}
                api={STT_MODEL_API}
              />
              <ModelPicker
                icon={Layers}
                title={t('embTitle')}
                description={t('embDesc')}
                api={EMB_MODEL_API}
              />
              <PerformanceCard />
            </div>
          </SectionBlock>

          <SectionBlock title={t('sectionData')} delay="80ms">
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4 items-start">
              <AssetTypesCard prefs={prefs} setType={setType} />
              <DataActions onChanged={onDataCleared} />
            </div>
          </SectionBlock>

          <SectionBlock title={t('sectionUpdates')} delay="120ms">
            <div className="flex flex-col gap-4">
              <UpdateChannelPicker />
              <RuntimeBinariesCard />
            </div>
          </SectionBlock>

          <SectionBlock title={t('sectionDanger')} delay="160ms">
            <div className="rounded-xl border border-red-900/40 bg-gradient-to-b from-[#160c0c] to-[#141010] overflow-hidden">
              <div className="flex items-start gap-3 px-5 py-4 border-b border-red-900/30">
                <ShieldAlert size={18} className="text-red-500 mt-0.5 shrink-0" />
                <div>
                  <p className="text-white text-sm font-medium">{t('dangerHeading')}</p>
                  <p className="text-gray-500 text-xs mt-1 leading-relaxed">
                    {t('dangerSubheading')}
                  </p>
                </div>
              </div>

              <div className="px-5 py-1 divide-y divide-red-900/20">
                <DangerRow
                  title={t('dangerAssetsTitle')}
                  desc={t('dangerAssetsDesc')}
                  buttonLabel={t('dangerAssetsButton')}
                  busyLabel={tcDeleting}
                  doneLabel={t('dangerAssetsDone')}
                  onConfirm={async () => {
                    await window.electronAPI.clearAllAssets();
                    onDataCleared?.();
                  }}
                />

                <DangerRow
                  title={t('dangerAiTitle')}
                  desc={t('dangerAiDesc')}
                  buttonLabel={t('dangerAiButton')}
                  busyLabel={tcDeleting}
                  doneLabel={t('dangerAiDone')}
                  onConfirm={async () => {
                    await window.electronAPI.clearAllAiAnalysis();
                    onDataCleared?.();
                  }}
                />

                <DangerRow
                  title={t('dangerDataTitle')}
                  desc={t('dangerDataDesc')}
                  buttonLabel={t('dangerDataButton')}
                  busyLabel={tcDeleting}
                  doneLabel={t('dangerDataDone')}
                  onConfirm={async () => {
                    await window.electronAPI.clearAllData();
                    onDataCleared?.();
                  }}
                />
              </div>
            </div>
          </SectionBlock>

          <SectionBlock title={t('sectionLegal')} delay="200ms">
            <LegalCard />
          </SectionBlock>
        </div>
      </div>
    </div>
  );
}
