import { useCallback, useEffect, useRef, useState } from 'react';
import type { AiDictationApi } from '../api/ai';
import { DictationRecorder } from '../lib/dictation/recorder';
import { useT } from '../i18n';
import { useFailureText } from './useFailureText';
import type { UseDictationOpts, UseDictationResult } from './useDictation';
interface Recognition {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  onresult: ((event: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => void) | null;
  onerror: (() => void) | null;
  start(): void;
  abort(): void;
}
type RecognitionConstructor = new () => Recognition;
function speechConstructor(): RecognitionConstructor | undefined {
  const browser = window as unknown as {
    SpeechRecognition?: RecognitionConstructor;
    webkitSpeechRecognition?: RecognitionConstructor;
  };
  return browser.SpeechRecognition ?? browser.webkitSpeechRecognition;
}
const MIN_SECONDS = 0.4;
const MAX_MS = 120_000;
/** Web-only lifecycle: one final request, no local model or rolling upload. */
export function useWebDictation(
  api: AiDictationApi | undefined,
  { onResult, language }: UseDictationOpts,
): UseDictationResult {
  const t = useT('dictation');
  const failure = useFailureText();
  const [status, setStatus] = useState<UseDictationResult['status']>('idle');
  const [error, setError] = useState<string | null>(null);
  const [liveText, setLiveText] = useState('');
  const [notice, setNotice] = useState(false);
  const statusRef = useRef(status);
  const session = useRef(0);
  const previousApi = useRef(api);
  const recorder = useRef<DictationRecorder | null>(null);
  const request = useRef<AbortController | null>(null);
  const speech = useRef<Recognition | null>(null);
  const allowedInterim = useRef(false);
  const maxTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const levelTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const level = useRef(0);
  const subscribers = useRef(new Set<(value: number) => void>());
  const onResultRef = useRef(onResult);
  onResultRef.current = onResult;
  const stopping = useRef<Promise<string> | null>(null);
  const updateStatus = useCallback((value: UseDictationResult['status']): void => {
    statusRef.current = value;
    setStatus(value);
  }, []);
  const updateLevel = useCallback((value: number): void => {
    level.current = value;
    subscribers.current.forEach((cb) => cb(value));
  }, []);
  const cleanup = useCallback((): void => {
    if (maxTimer.current) clearTimeout(maxTimer.current);
    if (levelTimer.current) clearInterval(levelTimer.current);
    maxTimer.current = null;
    levelTimer.current = null;
    if (speech.current) {
      speech.current.onresult = null;
      speech.current.onerror = null;
      try {
        speech.current.abort();
      } catch {}
    }
    speech.current = null;
    recorder.current?.stop();
    recorder.current = null;
    updateLevel(0);
  }, [updateLevel]);
  const invalidate = useCallback((): void => {
    session.current++;
    request.current?.abort();
    cleanup();
  }, [cleanup]);
  const cancel = useCallback((): void => {
    session.current++;
    request.current?.abort();
    request.current = null;
    cleanup();
    stopping.current = null;
    allowedInterim.current = false;
    setNotice(false);
    setLiveText('');
    setError(null);
    updateStatus('idle');
  }, [cleanup, updateStatus]);
  const stop = useCallback(
    ({ silent = false }: { silent?: boolean } = {}): Promise<string> => {
      if (stopping.current) return stopping.current;
      const rec = recorder.current;
      if (!rec || statusRef.current !== 'recording') {
        cancel();
        return Promise.resolve('');
      }
      const current = ++session.current;
      const wav = rec.hasAudio && rec.durationSec >= MIN_SECONDS ? rec.getWavSnapshot() : null;
      // Release mic and browser speech service before waiting for the server.
      cleanup();
      allowedInterim.current = false;
      setLiveText('');
      updateStatus('transcribing');
      const abort = new AbortController();
      request.current = abort;
      const operation = Promise.resolve().then(async (): Promise<string> => {
        try {
          if (!wav) {
            setError(t('emptyRecording'));
            updateStatus('error');
            return '';
          }
          const result = await api!.transcribe(wav, { language, signal: abort.signal });
          if (current !== session.current || abort.signal.aborted) return '';
          if (result.error) throw new Error('transcription failed');
          const text = (result.text ?? '').trim();
          updateStatus('idle');
          if (text && !silent) onResultRef.current?.(text);
          return text;
        } catch (cause) {
          if (current !== session.current || abort.signal.aborted) return '';
          setError(failure(cause));
          updateStatus('error');
          return '';
        } finally {
          if (current === session.current) {
            request.current = null;
            stopping.current = null;
          }
        }
      });
      stopping.current = operation;
      return operation;
    },
    [api, cancel, cleanup, failure, language, t, updateStatus],
  );
  const start = useCallback(async (): Promise<void> => {
    if (!api || (statusRef.current !== 'idle' && statusRef.current !== 'error')) return;
    const current = ++session.current;
    setError(null);
    setLiveText('');
    updateStatus('requesting');
    try {
      await api.ensure();
      if (current !== session.current) return;
      const interim = !!speechConstructor() && (await api.interimEnabled?.()) === true;
      if (current !== session.current) return;
      if (interim && !allowedInterim.current) {
        setNotice(true);
        updateStatus('idle');
        return;
      }
      setNotice(false);
      const rec = new DictationRecorder({ maxSeconds: 120, rolling: false });
      recorder.current = rec;
      await rec.start();
      if (current !== session.current) {
        rec.stop();
        return;
      }
      updateStatus('recording');
      levelTimer.current = setInterval(() => updateLevel(rec.level), 100);
      maxTimer.current = setTimeout(() => void stop(), MAX_MS);
      if (interim && allowedInterim.current) {
        const Recognition = speechConstructor()!;
        const recognizer = new Recognition();
        speech.current = recognizer;
        recognizer.lang = language ?? 'it';
        recognizer.continuous = true;
        recognizer.interimResults = true;
        recognizer.onresult = (event) => {
          if (current !== session.current) return;
          setLiveText(
            Array.from(event.results)
              .map((result) => result[0]?.transcript ?? '')
              .join(' '),
          );
        };
        recognizer.onerror = () => {}; // Final WAV transcription remains available.
        try {
          recognizer.start();
        } catch {
          recognizer.abort();
          speech.current = null;
        }
      }
    } catch (cause) {
      if (current !== session.current) return;
      cleanup();
      allowedInterim.current = false;
      const name = cause && typeof cause === 'object' && 'name' in cause ? String(cause.name) : '';
      setError(
        t(
          name === 'NotAllowedError' || name === 'SecurityError'
            ? 'permissionDeniedWeb'
            : name === 'NotFoundError'
              ? 'microphoneMissing'
              : name === 'NotReadableError'
                ? 'microphoneBusy'
                : 'startFailed',
        ),
      );
      updateStatus('error');
    }
  }, [api, cleanup, language, stop, t, updateLevel, updateStatus]);
  const acceptInterimNotice = useCallback(async (): Promise<void> => {
    allowedInterim.current = true;
    setNotice(false);
    await start();
  }, [start]);
  const dismissInterimNotice = useCallback((): void => {
    allowedInterim.current = false;
    setNotice(false);
  }, []);
  useEffect(() => {
    if (previousApi.current !== api) cancel();
    previousApi.current = api;
    return invalidate;
  }, [api, cancel, invalidate]);
  return {
    status,
    isActive: status === 'requesting' || status === 'recording' || status === 'transcribing',
    liveText,
    error,
    getAudioLevel: () => level.current,
    subscribeAudioLevel: (cb) => {
      subscribers.current.add(cb);
      return () => {
        subscribers.current.delete(cb);
      };
    },
    modelStatus: { ready: true },
    modelProgress: null,
    downloadModel: async () => {},
    start,
    stop,
    toggle: () => {
      if (statusRef.current === 'recording') void stop();
      else if (statusRef.current === 'transcribing' || statusRef.current === 'requesting') cancel();
      else void start();
    },
    cancel,
    interimNoticeRequired: notice,
    acceptInterimNotice,
    dismissInterimNotice,
  };
}
