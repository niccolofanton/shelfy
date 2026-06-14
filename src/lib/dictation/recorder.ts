// Microphone capture for voice dictation. Records mono 16kHz PCM via an
// AudioWorklet, keeps a rolling buffer (capped to MAX_SECONDS), and can snapshot
// it to a 16-bit WAV at any moment for whisper.cpp's /inference endpoint.

// The worklet is authored in TS but must reach the browser as real JavaScript:
// a bare `new URL('./pcm-worklet.ts', import.meta.url)` makes Vite emit the file
// untranspiled under a `video/mp2t` MIME (from the .ts extension), which
// audioWorklet.addModule() rejects. `?worker&url` makes Vite transpile + emit it
// as a hashed .js asset and hand back its URL — the load-mechanism trick for
// worklets (the code uses no Worker APIs, only registerProcessor).
import pcmWorkletUrl from './pcm-worklet.ts?worker&url';

export const SAMPLE_RATE = 16000;
const MAX_SECONDS = 60;
const MAX_SAMPLES = SAMPLE_RATE * MAX_SECONDS;

// Encodes Float32 mono samples [-1,1] into a 16-bit PCM WAV ArrayBuffer.
export function encodeWav(samples: Float32Array, sampleRate: number = SAMPLE_RATE): ArrayBuffer {
  const numFrames = samples.length;
  const buffer = new ArrayBuffer(44 + numFrames * 2);
  const view = new DataView(buffer);
  const writeStr = (off: number, s: string): void => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };

  writeStr(0, 'RIFF');
  view.setUint32(4, 36 + numFrames * 2, true);
  writeStr(8, 'WAVE');
  writeStr(12, 'fmt ');
  view.setUint32(16, 16, true); // PCM fmt chunk size
  view.setUint16(20, 1, true); // audio format = PCM
  view.setUint16(22, 1, true); // channels = mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate = sr * blockAlign
  view.setUint16(32, 2, true); // block align = channels * bytes/sample
  view.setUint16(34, 16, true); // bits per sample
  writeStr(36, 'data');
  view.setUint32(40, numFrames * 2, true);

  let off = 44;
  for (let i = 0; i < numFrames; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(off, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    off += 2;
  }
  return buffer;
}

export class DictationRecorder {
  private _chunks: Float32Array[];
  private _total: number;
  private _level: number;
  private _stream: MediaStream | null;
  private _ctx: AudioContext | null;
  private _node: AudioWorkletNode | null;
  private _gain: GainNode | null;

  constructor() {
    this._chunks = [];
    this._total = 0;
    this._level = 0;
    this._stream = null;
    this._ctx = null;
    this._node = null;
    this._gain = null;
  }

  async start(): Promise<void> {
    this._stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    this._ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
    await this._ctx.audioWorklet.addModule(pcmWorkletUrl);

    const src = this._ctx.createMediaStreamSource(this._stream);
    this._node = new AudioWorkletNode(this._ctx, 'pcm-recorder');
    this._node.port.onmessage = (e: MessageEvent<Float32Array>): void => this._onSamples(e.data);

    // A muted gain node gives the worklet a path to the destination so the graph
    // keeps pulling it, without playing the mic back (which would echo).
    this._gain = this._ctx.createGain();
    this._gain.gain.value = 0;
    src.connect(this._node);
    this._node.connect(this._gain);
    this._gain.connect(this._ctx.destination);
  }

  private _onSamples(samples: Float32Array): void {
    this._chunks.push(samples);
    this._total += samples.length;

    let sum = 0;
    for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i];
    this._level = Math.sqrt(sum / samples.length);

    // Rolling cap: drop the oldest chunks once past MAX_SAMPLES.
    while (this._total > MAX_SAMPLES && this._chunks.length > 1) {
      this._total -= (this._chunks.shift() as Float32Array).length;
    }
  }

  get level(): number {
    return this._level;
  }
  get durationSec(): number {
    return this._total / SAMPLE_RATE;
  }
  get hasAudio(): boolean {
    return this._total > 0;
  }

  getWavSnapshot(): ArrayBuffer {
    const merged = new Float32Array(this._total);
    let offset = 0;
    for (const c of this._chunks) {
      merged.set(c, offset);
      offset += c.length;
    }
    return encodeWav(merged, SAMPLE_RATE);
  }

  stop(): void {
    try {
      if (this._node) this._node.port.onmessage = null;
    } catch {}
    try {
      this._node?.disconnect();
    } catch {}
    try {
      this._gain?.disconnect();
    } catch {}
    try {
      this._stream?.getTracks().forEach((t) => t.stop());
    } catch {}
    try {
      this._ctx?.close();
    } catch {}
    this._node = null;
    this._gain = null;
    this._stream = null;
    this._ctx = null;
  }
}
