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
];

// Probe everything in parallel. Each call is best-effort: outside Electron (or on
// an old preload) a missing/throwing API resolves to null and the piece simply
// reads as not-ready. Returns null only when the whole bridge is absent.
export async function fetchAiSetup() {
  const api = window.electronAPI;
  if (!api?.listModels) return null;
  const safe = (p) => Promise.resolve(p).catch(() => null);
  const [binaries, vlm, stt, emb] = await Promise.all([
    safe(api.getBinariesStatus?.()),
    safe(api.listModels?.()),
    safe(api.sttListModels?.()),
    safe(api.embListModels?.()),
  ]);
  const anyReady = (list) => Array.isArray(list) && list.some((m) => m?.ready);
  const status = {
    binaries: binaries || null,
    vlm: Array.isArray(vlm) ? vlm : [],
    stt: Array.isArray(stt) ? stt : [],
    emb: Array.isArray(emb) ? emb : [],
  };
  status.complete = !!binaries?.ready && anyReady(vlm) && anyReady(stt) && anyReady(emb);
  return status;
}

// Lightweight gate hook for App: exposes only coarse readiness (no download
// progress — that stays inside AiOnboarding so App doesn't re-render per chunk).
export function useAiSetupStatus() {
  const [status, setStatus] = useState(null); // null = still probing

  const refresh = useCallback(async () => {
    const s = await fetchAiSetup();
    if (s) setStatus(s);
    return s;
  }, []);

  useEffect(() => {
    refresh();
    const onChanged = () => refresh();
    CHANGE_EVENTS.forEach((e) => window.addEventListener(e, onChanged));
    return () => CHANGE_EVENTS.forEach((e) => window.removeEventListener(e, onChanged));
  }, [refresh]);

  return { status, loading: status == null, complete: !!status?.complete, refresh };
}
