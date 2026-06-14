import { useCallback, useEffect, useState } from 'react';

// ── AI setup status ─────────────────────────────────────────────────────────────
// Single source of truth for "is the local AI pipeline ready to run?". The four
// pieces the analysis pipeline needs are probed via the same IPC surface Settings
// uses, so this never duplicates state — it just reads what's on disk:
//   1. runtime binaries (llama-server, whisper-server, ffmpeg)
//   2. a vision/analysis model (any VLM preset downloaded)
//   3. a voice/dictation model (any whisper preset downloaded)
//   4. the tag-clustering embedding model (e5-small)
// App.jsx gates the AI tabs on `complete`; the AiOnboarding wizard drives the
// missing pieces to ready and then re-emits `ai-setup-changed`.

// Re-check readiness whenever any model family changes (the Settings pickers
// dispatch the first three; the onboarding dispatches the last after each step).
const CHANGE_EVENTS = [
  'ai-model-changed',
  'stt-model-changed',
  'emb-model-changed',
  'ai-setup-changed',
] as const;

// Runtime-binaries status shape: only `ready` is read here (binaries.* return a
// file-internal shape typed `unknown` on the bridge).
interface BinariesStatus {
  ready?: boolean;
  [key: string]: unknown;
}

// A model preset row from list*Models(): only `ready` is read here.
interface ModelEntry {
  ready?: boolean;
  [key: string]: unknown;
}

// The aggregate readiness snapshot fetchAiSetup produces.
export interface AiSetupStatus {
  binaries: BinariesStatus | null;
  vlm: ModelEntry[];
  stt: ModelEntry[];
  emb: ModelEntry[];
  complete: boolean;
}

// Narrow an unknown bridge value to a BinariesStatus (just an object or null).
function asBinaries(v: unknown): BinariesStatus | null {
  return v && typeof v === 'object' ? (v as BinariesStatus) : null;
}

// Narrow an unknown bridge value to a ModelEntry list.
function asModelList(v: unknown): ModelEntry[] {
  return Array.isArray(v) ? (v as ModelEntry[]) : [];
}

// Probe everything in parallel. Each call is best-effort: outside Electron (or on
// an old preload) a missing/throwing API resolves to null and the piece simply
// reads as not-ready. Returns null only when the whole bridge is absent.
export async function fetchAiSetup(): Promise<AiSetupStatus | null> {
  const api = window.electronAPI;
  if (!api?.listModels) return null;
  const safe = <T>(p: Promise<T> | undefined): Promise<T | null> =>
    Promise.resolve(p as Promise<T>).catch(() => null);
  const [binaries, vlm, stt, emb] = await Promise.all([
    safe(api.getBinariesStatus?.()),
    safe(api.listModels?.()),
    safe(api.sttListModels?.()),
    safe(api.embListModels?.()),
  ]);
  const bin = asBinaries(binaries);
  const vlmList = asModelList(vlm);
  const sttList = asModelList(stt);
  const embList = asModelList(emb);
  const anyReady = (list: ModelEntry[]): boolean => list.some((m) => !!m?.ready);
  const status: AiSetupStatus = {
    binaries: bin,
    vlm: vlmList,
    stt: sttList,
    emb: embList,
    complete: !!bin?.ready && anyReady(vlmList) && anyReady(sttList) && anyReady(embList),
  };
  return status;
}

export interface UseAiSetupStatusResult {
  status: AiSetupStatus | null;
  loading: boolean;
  complete: boolean;
  refresh: () => Promise<AiSetupStatus | null>;
}

// Lightweight gate hook for App: exposes only coarse readiness (no download
// progress — that stays inside AiOnboarding so App doesn't re-render per chunk).
export function useAiSetupStatus(): UseAiSetupStatusResult {
  const [status, setStatus] = useState<AiSetupStatus | null>(null); // null = still probing

  const refresh = useCallback(async (): Promise<AiSetupStatus | null> => {
    const s = await fetchAiSetup();
    if (s) setStatus(s);
    return s;
  }, []);

  useEffect(() => {
    refresh();
    const onChanged = (): void => {
      refresh();
    };
    CHANGE_EVENTS.forEach((e) => window.addEventListener(e, onChanged));
    return () => CHANGE_EVENTS.forEach((e) => window.removeEventListener(e, onChanged));
  }, [refresh]);

  return { status, loading: status == null, complete: !!status?.complete, refresh };
}
