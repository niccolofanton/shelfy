// Voice dictation for the AI-search composer (web port plan §2.15, §2.3,
// §1.2 #9). Maps to `POST /stt/transcriptions`: the web records a WAV with
// the desktop's AudioWorklet recorder and sends one request per recording
// (≤120s, P3-17); the desktop keeps its local whisper.cpp loop (the model
// readiness/download lifecycle below is desktop-only — a BYOK/operator STT
// route on the web is always "ready").
export interface AiDictationApi {
  mode?: 'web';
  interimEnabled?(): Promise<boolean>;
  // The local model's readiness (desktop: whisper.cpp's model + binary state).
  status(): Promise<unknown>;
  onModelProgress(cb: (progress: unknown) => void): () => void;
  downloadModel(): Promise<unknown>;
  // Ensures the model/binary are loaded before the first recording of a
  // session (desktop only; a no-op once the web's STT route always answers).
  ensure(): Promise<void>;
  // One recording in, `{text}` out.
  transcribe(
    wav: ArrayBuffer | ArrayBufferView,
    opts?: { language?: string; signal?: AbortSignal },
  ): Promise<{ text?: string; error?: string }>;
}
