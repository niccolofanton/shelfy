import { useCallback, useEffect, useRef, useState } from 'react';
import { DictationRecorder } from '../lib/dictation/recorder';
import { translate, getInitialLang } from '../i18n';

// User-facing error messages surfaced via `error` (rendered in the AiSearch
// composer). Resolved against the persisted language; the whisper `language`
// param and status enums stay as-is.
const td = (key) => translate(getInitialLang(), `dictation.${key}`);

// Voice dictation for the AI-search composer. Captures the mic, re-transcribes
// the growing buffer every TICK_MS via the local whisper.cpp server (so the text
// updates live as you speak), and on stop hands the final text to `onResult`.
//
// Model lifecycle mirrors useAnalysis: `modelStatus` + `modelProgress` are tracked
// independently of recording, and the download is an explicit action (driven by a
// banner), not something that fires on the first mic click.
//
// status: 'idle' | 'requesting' | 'recording' | 'transcribing' | 'error'

const TICK_MS = 1200; // how often we re-transcribe the rolling buffer
const MIN_SECONDS = 0.4; // skip transcribing buffers too short to be useful

export function useDictation({ onResult, language = 'it' } = {}) {
  const [status, setStatus] = useState('idle');
  const [liveText, setLiveText] = useState('');
  const [error, setError] = useState(null);
  const [modelStatus, setModelStatus] = useState(null); // { modelReady, binaryReady, ready, downloading }
  const [modelProgress, setModelProgress] = useState(null); // { progress, label } during download

  const recRef = useRef(null);
  const tickRef = useRef(null);
  const levelRef = useRef(null);
  const inFlightRef = useRef(false);
  const sessionRef = useRef(0);
  const liveTextRef = useRef('');
  const onResultRef = useRef(onResult);
  onResultRef.current = onResult;

  // Audio level is updated ~10x/s while recording. Keeping it in React state
  // would re-render the ENTIRE consumer (and its unmemoized results grid) on
  // every tick. Instead store it in a ref and notify only explicit subscribers
  // (a tiny leaf that drives the mic glow), so high-frequency updates never
  // touch the main view's render path.
  const audioLevelRef = useRef(0);
  const audioSubsRef = useRef(new Set());
  const setAudioLevel = useCallback((lvl) => {
    audioLevelRef.current = lvl;
    audioSubsRef.current.forEach((cb) => cb(lvl));
  }, []);
  const subscribeAudioLevel = useCallback((cb) => {
    audioSubsRef.current.add(cb);
    return () => audioSubsRef.current.delete(cb);
  }, []);
  const getAudioLevel = useCallback(() => audioLevelRef.current, []);

  const refreshModel = useCallback(async () => {
    if (!window.electronAPI?.sttStatus) return null;
    try {
      const s = await window.electronAPI.sttStatus();
      setModelStatus(s);
      return s;
    } catch (err) {
      console.error('[useDictation] sttStatus error:', err);
      return null;
    }
  }, []);

  // Mount: load model status + subscribe to download progress (mirrors useAnalysis).
  useEffect(() => {
    refreshModel();
    if (!window.electronAPI?.onSttModelProgress) return undefined;
    const unsub = window.electronAPI.onSttModelProgress((p) => {
      setModelProgress(p);
      if (p?.progress >= 1) {
        setModelProgress(null);
        refreshModel();
      }
    });
    return () => {
      if (typeof unsub === 'function') unsub();
    };
  }, [refreshModel]);

  // Explicit model download (mirrors useAnalysis.downloadModel): the banner calls
  // this; progress is reflected via modelProgress until the file is present.
  const downloadModel = useCallback(async () => {
    if (!window.electronAPI?.sttDownloadModel) return;
    setModelProgress({ progress: 0, label: 'voce' });
    try {
      await window.electronAPI.sttDownloadModel();
    } catch (err) {
      console.error('[useDictation] sttDownloadModel error:', err);
    } finally {
      setModelProgress(null);
      refreshModel();
    }
  }, [refreshModel]);

  const setLive = useCallback((t) => {
    liveTextRef.current = t;
    setLiveText(t);
  }, []);

  const clearTimers = useCallback(() => {
    if (tickRef.current) {
      clearInterval(tickRef.current);
      tickRef.current = null;
    }
    if (levelRef.current) {
      clearInterval(levelRef.current);
      levelRef.current = null;
    }
  }, []);

  const transcribeSnapshot = useCallback(
    async (session) => {
      const rec = recRef.current;
      if (!rec || !rec.hasAudio || rec.durationSec < MIN_SECONDS) return '';
      const wav = rec.getWavSnapshot();
      const res = await window.electronAPI.sttTranscribe(wav, { language });
      if (session !== sessionRef.current) return ''; // a newer session superseded us
      return res?.text ?? '';
    },
    [language],
  );

  // Stops recording and returns the final transcript. By default it also pushes
  // the text via onResult (populates the composer). Pass { silent: true } to get
  // the text back WITHOUT firing onResult — used when sending straight away, so
  // the caller composes/clears the draft itself instead of double-writing it.
  const stop = useCallback(
    async ({ silent = false } = {}) => {
      // Bump the session FIRST so any tick transcription already in flight is
      // invalidated by its own session check (otherwise a late tick could call
      // setLive after we clear it below). The final transcribe runs under the new
      // session id so its result is still accepted.
      const session = ++sessionRef.current;
      clearTimers();
      const rec = recRef.current;
      if (!rec) {
        setStatus('idle');
        return '';
      }
      setStatus('transcribing');
      let finalText = liveTextRef.current;
      try {
        const text = await transcribeSnapshot(session);
        if (text) finalText = text;
      } catch (err) {
        console.error('[useDictation] final transcribe error:', err);
      }
      try {
        rec.stop();
      } catch {}
      recRef.current = null;
      setAudioLevel(0);
      setLive('');
      setStatus('idle');
      const out = (finalText || '').trim();
      if (out && !silent) onResultRef.current?.(out);
      return out;
    },
    [clearTimers, transcribeSnapshot, setLive, setAudioLevel],
  );

  const start = useCallback(async () => {
    if (status !== 'idle' && status !== 'error') return;

    // The model + binary must be ready; the banner owns downloading. Re-check in
    // case status is stale.
    const st = modelStatus?.ready ? modelStatus : await refreshModel();
    if (!st?.ready) {
      setError(st && st.binaryReady === false ? td('binaryMissing') : td('modelNotReady'));
      setStatus('error');
      return;
    }

    setError(null);
    setLive('');
    const session = ++sessionRef.current;

    try {
      setStatus('requesting');
      await window.electronAPI.sttEnsure();
      if (session !== sessionRef.current) return;

      const rec = new DictationRecorder();
      await rec.start();
      if (session !== sessionRef.current) {
        rec.stop();
        return;
      }
      recRef.current = rec;
      setStatus('recording');

      levelRef.current = setInterval(() => setAudioLevel(rec.level), 100);
      tickRef.current = setInterval(async () => {
        if (inFlightRef.current) return; // never overlap transcriptions
        inFlightRef.current = true;
        try {
          const text = await transcribeSnapshot(session);
          if (text && session === sessionRef.current) setLive(text);
        } catch (err) {
          console.error('[useDictation] tick transcribe error:', err);
        } finally {
          inFlightRef.current = false;
        }
      }, TICK_MS);
    } catch (err) {
      console.error('[useDictation] start error:', err);
      sessionRef.current++; // invalidate any late callbacks
      clearTimers();
      try {
        recRef.current?.stop();
      } catch {}
      recRef.current = null;
      setError(err?.name === 'NotAllowedError' ? td('permissionDenied') : td('startFailed'));
      setStatus('error');
    }
  }, [status, modelStatus, refreshModel, setLive, clearTimers, transcribeSnapshot, setAudioLevel]);

  const toggle = useCallback(() => {
    if (status === 'recording') stop();
    else if (status === 'idle' || status === 'error') start();
  }, [status, start, stop]);

  // Tear down on unmount.
  useEffect(
    () => () => {
      sessionRef.current++;
      clearTimers();
      try {
        recRef.current?.stop();
      } catch {}
      recRef.current = null;
    },
    [clearTimers],
  );

  const isActive = status === 'requesting' || status === 'recording' || status === 'transcribing';

  return {
    status,
    isActive,
    liveText,
    // Audio level is exposed via a ref-getter + subscriber rather than as React
    // state, so the consumer's main render isn't driven at ~10Hz. A leaf
    // component subscribes to drive the mic glow in isolation.
    getAudioLevel,
    subscribeAudioLevel,
    error,
    modelStatus,
    modelProgress,
    downloadModel,
    start,
    stop,
    toggle,
  };
}

export default useDictation;
