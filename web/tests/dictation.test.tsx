import React from 'react';
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AiDictationApi } from '../../src/api/ai';
import { I18nProvider } from '../../src/i18n';
import { useDictation } from '../../src/hooks/useDictation';
import { useWebDictation } from '../../src/hooks/useWebDictation';
import { createDictationApi } from '../src/api/ai/dictation';
import { createHttp, ApiError } from '../src/api/http';
const client = vi.hoisted(() => ({ ai: undefined as { dictation: AiDictationApi } | undefined }));
vi.mock('../../src/api/ShelfyProvider', () => ({ useShelfy: () => client }));
const recorder = vi.hoisted(() => ({
  start: vi.fn(),
  stop: vi.fn(),
  wav: new ArrayBuffer(100),
  seconds: 1,
  instances: 0,
  options: undefined as unknown,
}));
vi.mock('../../src/lib/dictation/recorder', () => ({
  DictationRecorder: class {
    constructor(options: unknown) {
      recorder.instances++;
      recorder.options = options;
    }
    start() {
      return recorder.start();
    }
    stop() {
      recorder.stop();
    }
    get durationSec() {
      return recorder.seconds;
    }
    get hasAudio() {
      return recorder.seconds > 0;
    }
    get level() {
      return 0.2;
    }
    getWavSnapshot() {
      return recorder.wav;
    }
  },
}));
function wrapper({ children }: { children: React.ReactNode }) {
  return <I18nProvider>{children}</I18nProvider>;
}
function fixture() {
  const api: AiDictationApi = {
    mode: 'web',
    status: async () => ({ ready: true }),
    onModelProgress: () => () => {},
    downloadModel: async () => {},
    ensure: vi.fn(async () => {}),
    interimEnabled: vi.fn(async () => false),
    transcribe: vi.fn(async () => ({ text: ' final synthetic text ' })),
  };
  const onResult = vi.fn();
  const hook = renderHook(() => useWebDictation(api, { language: 'en', onResult }), { wrapper });
  return { ...hook, api, onResult };
}
beforeEach(() => {
  localStorage.setItem('app:language', 'en');
  recorder.start.mockResolvedValue(undefined);
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  recorder.seconds = 1;
  recorder.instances = 0;
  recorder.start.mockResolvedValue(undefined);
});
describe('web dictation', () => {
  it('records without rolling uploads; releases mic and transcribes once on stop', async () => {
    const { result, api, onResult } = fixture();
    await act(async () => result.current.start());
    expect(result.current.status).toBe('recording');
    expect(recorder.options).toEqual({ maxSeconds: 120, rolling: false });
    expect(api.transcribe).not.toHaveBeenCalled();
    let text = '';
    await act(async () => {
      text = await result.current.stop();
    });
    expect(recorder.stop).toHaveBeenCalledOnce();
    expect(api.transcribe).toHaveBeenCalledOnce();
    expect(api.transcribe).toHaveBeenCalledWith(recorder.wav, {
      language: 'en',
      signal: expect.any(AbortSignal),
    });
    expect(text).toBe('final synthetic text');
    expect(onResult).toHaveBeenCalledWith(text);
    expect(result.current.status).toBe('idle');
  });
  it('auto-stops at 120 seconds and returns silent final text without editing the composer', async () => {
    vi.useFakeTimers();
    const { result, api } = fixture();
    await act(async () => result.current.start());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(119_999);
    });
    expect(api.transcribe).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(api.transcribe).toHaveBeenCalledOnce();
    const second = fixture();
    await act(async () => second.result.current.start());
    await act(async () => {
      expect(await second.result.current.stop({ silent: true })).toBe('final synthetic text');
    });
    expect(second.onResult).not.toHaveBeenCalled();
  });
  it('cancel aborts pending transcription and suppresses late results, unmount stops capture', async () => {
    const f = fixture();
    let resolve!: (value: { text: string }) => void;
    vi.mocked(f.api.transcribe).mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    await act(async () => f.result.current.start());
    let pending!: Promise<string>;
    await act(async () => {
      pending = f.result.current.stop();
      await Promise.resolve();
    });
    const signal = vi.mocked(f.api.transcribe).mock.calls[0][1]!.signal!;
    act(() => f.result.current.cancel!());
    expect(signal.aborted).toBe(true);
    await act(async () => {
      resolve({ text: 'late text' });
      expect(await pending).toBe('');
    });
    expect(f.onResult).not.toHaveBeenCalled();
    await act(async () => f.result.current.start());
    f.unmount();
    expect(recorder.stop).toHaveBeenCalledTimes(2);
  });
  it('does not restore an empty-recording error after same-tick cancel or unmount', async () => {
    const f = fixture();
    recorder.seconds = 0;
    await act(async () => f.result.current.start());
    await act(async () => {
      const pending = f.result.current.stop();
      f.result.current.cancel!();
      expect(await pending).toBe('');
    });
    expect(f.result.current.status).toBe('idle');
    expect(f.result.current.error).toBeNull();
    expect(f.api.transcribe).not.toHaveBeenCalled();
    await act(async () => f.result.current.start());
    await act(async () => {
      const pending = f.result.current.stop();
      f.unmount();
      expect(await pending).toBe('');
    });
    expect(f.api.transcribe).not.toHaveBeenCalled();
    expect(f.onResult).not.toHaveBeenCalled();
  });
  it('cancels a delayed permission prompt and closes the late recorder', async () => {
    let grant!: () => void;
    recorder.start.mockImplementation(
      () =>
        new Promise<void>((done) => {
          grant = done;
        }),
    );
    const f = fixture();
    let pending!: Promise<void>;
    await act(async () => {
      pending = f.result.current.start();
      await Promise.resolve();
      await Promise.resolve();
    });
    act(() => f.result.current.cancel!());
    await act(async () => {
      grant();
      await pending;
    });
    expect(f.result.current.status).toBe('idle');
    expect(f.api.transcribe).not.toHaveBeenCalled();
    expect(recorder.stop).toHaveBeenCalledTimes(2);
  });
  it('shows permission and provider errors; refuses a too-short recording', async () => {
    const f = fixture();
    recorder.start.mockRejectedValueOnce(new DOMException('synthetic denied', 'NotAllowedError'));
    await act(async () => f.result.current.start());
    expect(f.result.current.error).toContain('Microphone permission denied');
    vi.mocked(f.api.transcribe).mockRejectedValueOnce(new ApiError(503, 'provider_offline'));
    await act(async () => f.result.current.start());
    await act(async () => {
      await f.result.current.stop();
    });
    expect(f.result.current.error).toContain('cannot be reached');
    recorder.seconds = 0.1;
    await act(async () => f.result.current.start());
    await act(async () => {
      await f.result.current.stop();
    });
    expect(f.result.current.error).toContain('too short');
    expect(f.api.transcribe).toHaveBeenCalledOnce();
  });
  it('API replacement cancels an old account recording and resets the new account lifecycle', async () => {
    const f = fixture();
    const replacement = { ...f.api, transcribe: vi.fn(async () => ({ text: 'new account' })) };
    const hook = renderHook(({ api }) => useWebDictation(api, { language: 'en' }), {
      wrapper,
      initialProps: { api: f.api },
    });
    await act(async () => hook.result.current.start());
    hook.rerender({ api: replacement });
    expect(recorder.stop).toHaveBeenCalledOnce();
    expect(hook.result.current.status).toBe('idle');
    await act(async () => hook.result.current.start());
    await act(async () => {
      await hook.result.current.stop();
    });
    expect(f.api.transcribe).not.toHaveBeenCalled();
    expect(replacement.transcribe).toHaveBeenCalledOnce();
  });
  it('the desktop still snapshots every 1.2 seconds and uses its original final transcription', async () => {
    vi.useFakeTimers();
    const api: AiDictationApi = {
      status: async () => ({ ready: true }),
      ensure: async () => {},
      downloadModel: async () => {},
      onModelProgress: () => () => {},
      transcribe: vi.fn(async () => ({ text: 'desktop voice' })),
    };
    client.ai = { dictation: api };
    const onResult = vi.fn();
    const hook = renderHook(() => useDictation({ onResult, language: 'it' }), { wrapper });
    await act(async () => hook.result.current.start());
    expect(recorder.options).toBeUndefined();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_200);
    });
    expect(api.transcribe).toHaveBeenCalledOnce();
    await act(async () => {
      await hook.result.current.stop();
    });
    expect(api.transcribe).toHaveBeenCalledTimes(2);
    expect(onResult).toHaveBeenCalledWith('desktop voice');
    expect(api.transcribe).toHaveBeenLastCalledWith(recorder.wav, { language: 'it' });
    client.ai = undefined;
  });
  it('starts browser speech only after enabled setting AND explicit notice acceptance', async () => {
    const speechStart = vi.fn();
    const speechAbort = vi.fn();
    const recognitions: { onresult?: (event: unknown) => void }[] = [];
    vi.stubGlobal(
      'SpeechRecognition',
      class {
        constructor() {
          recognitions.push(this);
        }
        onresult?: (event: unknown) => void;
        start = speechStart;
        abort = speechAbort;
      },
    );
    const f = fixture();
    vi.mocked(f.api.interimEnabled!).mockResolvedValue(true);
    await act(async () => f.result.current.start());
    expect(f.result.current.interimNoticeRequired).toBe(true);
    expect(recorder.start).not.toHaveBeenCalled();
    expect(speechStart).not.toHaveBeenCalled();
    act(() => f.result.current.dismissInterimNotice!());
    expect(f.result.current.interimNoticeRequired).toBe(false);
    await act(async () => f.result.current.start());
    await act(async () => f.result.current.acceptInterimNotice!());
    expect(speechStart).toHaveBeenCalledOnce();
    const receive = recognitions[0].onresult!;
    act(() => receive({ results: [[{ transcript: 'synthetic interim' }]] }));
    expect(f.result.current.liveText).toBe('synthetic interim');
    act(() => f.result.current.cancel!());
    act(() => receive({ results: [[{ transcript: 'obsolete interim' }]] }));
    expect(f.result.current.liveText).toBe('');
    expect(speechAbort).toHaveBeenCalledOnce();
    vi.mocked(f.api.interimEnabled!).mockResolvedValue(false);
    await act(async () => f.result.current.start());
    expect(f.result.current.interimNoticeRequired).toBe(false);
    expect(speechStart).toHaveBeenCalledOnce();
  });
});
describe('dictation HTTP adapter', () => {
  it('sends exact bytes of a view with cookie/CSRF and language; cancellation is preserved', async () => {
    const fetch = vi.fn(
      async (_url: unknown, _init?: RequestInit) =>
        new Response(JSON.stringify({ text: 'stub transcript' }), { status: 200 }),
    );
    const api = createDictationApi(createHttp({ fetch: fetch as typeof window.fetch }));
    const audio = new Uint8Array([99, 1, 2, 3, 99]).subarray(1, 4);
    const abort = new AbortController();
    expect(await api.transcribe(audio, { language: 'en', signal: abort.signal })).toEqual({
      text: 'stub transcript',
    });
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe('/api/v1/stt/transcriptions?language=en');
    expect(init!.signal).toBe(abort.signal);
    expect(init!.credentials).toBe('same-origin');
    expect(init!.headers).toMatchObject({ 'Content-Type': 'audio/wav', 'X-Shelfy-Client': 'web' });
    expect(init!.body).toBeInstanceOf(Blob);
    // jsdom Blob lacks arrayBuffer; FileReader verifies the selected byte range.
    const bytes = await new Promise<ArrayBuffer>((resolve) => {
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result as ArrayBuffer);
      reader.readAsArrayBuffer(init!.body as Blob);
    });
    expect(Array.from(new Uint8Array(bytes))).toEqual([1, 2, 3]);
    fetch.mockImplementation(async (_url, init) => {
      if (init?.signal?.aborted) throw new DOMException('aborted', 'AbortError');
      return new Response();
    });
    abort.abort();
    await expect(api.transcribe(audio, { signal: abort.signal })).rejects.toMatchObject({
      name: 'AbortError',
    });
  });
});
