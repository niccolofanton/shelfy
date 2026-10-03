import type { AiDictationApi } from '@ui/api/ai';
import type { Http } from '../http';
// A WAV goes directly to Shelfy, never to a provider from this browser.
export function createDictationApi(http: Http): AiDictationApi {
  return {
    mode: 'web',
    async status() {
      return { ready: true };
    },
    onModelProgress() {
      return () => {};
    },
    async downloadModel() {},
    async ensure() {},
    async interimEnabled() {
      const settings = await http.get<{ aiDictationInterim?: boolean }>('/api/v1/me/settings');
      return settings.aiDictationInterim === true;
    },
    async transcribe(wav, opts) {
      const view =
        wav instanceof ArrayBuffer
          ? new Uint8Array(wav)
          : new Uint8Array(wav.buffer, wav.byteOffset, wav.byteLength);
      const blob = new Blob([Uint8Array.from(view)], { type: 'audio/wav' });
      const query = new URLSearchParams();
      if (opts?.language) query.set('language', opts.language);
      const response = await http.send(
        'POST',
        `/api/v1/stt/transcriptions${query.size ? `?${query}` : ''}`,
        undefined,
        { rawBody: blob, contentType: 'audio/wav', signal: opts?.signal },
      );
      return (await response.json()) as { text: string };
    },
  };
}
