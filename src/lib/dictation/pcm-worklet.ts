// AudioWorkletProcessor that forwards captured mono Float32 PCM to the main
// thread. Runs in the AudioWorkletGlobalScope — no imports allowed here.
// Buffers ~2048 samples per message to keep postMessage traffic low (the graph
// calls process() with 128-frame blocks).

// AudioWorkletGlobalScope globals are ambient here and not part of the DOM lib,
// so declare the minimal surface this processor uses without altering behavior.
declare abstract class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor();
  abstract process(
    inputs: Float32Array[][],
    outputs: Float32Array[][],
    parameters: Record<string, Float32Array>,
  ): boolean;
}

declare function registerProcessor(
  name: string,
  processorCtor: new () => AudioWorkletProcessor,
): void;

class PCMRecorderProcessor extends AudioWorkletProcessor {
  private _chunks: Float32Array[];
  private _count: number;
  private _target: number;

  constructor() {
    super();
    this._chunks = [];
    this._count = 0;
    this._target = 2048;
  }

  override process(inputs: Float32Array[][]): boolean {
    const input = inputs[0];
    const channel = input && input[0];
    if (channel && channel.length) {
      // The provided buffer is reused by the engine, so copy before keeping it.
      this._chunks.push(new Float32Array(channel));
      this._count += channel.length;
      if (this._count >= this._target) {
        const merged = new Float32Array(this._count);
        let offset = 0;
        for (const c of this._chunks) {
          merged.set(c, offset);
          offset += c.length;
        }
        this.port.postMessage(merged, [merged.buffer]);
        this._chunks = [];
        this._count = 0;
      }
    }
    return true; // keep the processor alive
  }
}

registerProcessor('pcm-recorder', PCMRecorderProcessor);
