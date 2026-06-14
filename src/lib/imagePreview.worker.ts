// Image preview downscaling off the UI thread. decode (createImageBitmap) +
// drawImage + WebP encode (OffscreenCanvas.convertToBlob) are CPU-bound and, on
// multi-MB images, freeze the modal while importing manual bookmarks. Running them
// here keeps the UI responsive. bookmarkFiles.js falls back to the main-thread
// path (which uses <Image>, so it also covers SVG that createImageBitmap rejects).

// Tenere in sync con bookmarkFiles.js (stessa resa visiva della preview).
const PREVIEW_MAX = 768; // longest preview edge, in px
const PREVIEW_QUALITY = 0.82;

// Inbound request from the main thread.
interface PreviewRequest {
  id: number;
  blob: Blob;
}

// Outbound result posted back to the main thread.
type PreviewResponse =
  | { id: number; ok: true; bytes: Uint8Array }
  | { id: number; ok: false; error: string };

// The DOM lib types `self` as a window-like scope whose postMessage signature
// requires a targetOrigin. Inside a dedicated worker it is the worker scope,
// which exposes the transfer-list overload we rely on for zero-copy results.
interface DedicatedWorkerScope {
  onmessage: ((this: DedicatedWorkerScope, ev: MessageEvent<unknown>) => void) | null;
  postMessage(message: PreviewResponse, transfer: Transferable[]): void;
}

const workerScope = self as unknown as DedicatedWorkerScope;

// Fit (w,h) inside a PREVIEW_MAX box, never upscaling.
function fitInside(w: number, h: number): { w: number; h: number } {
  const scale = Math.min(1, PREVIEW_MAX / Math.max(w, h));
  return { w: Math.max(1, Math.round(w * scale)), h: Math.max(1, Math.round(h * scale)) };
}

workerScope.onmessage = async (e: MessageEvent<unknown>): Promise<void> => {
  const { id, blob } = (e.data || {}) as Partial<PreviewRequest>;
  try {
    if (typeof createImageBitmap !== 'function' || typeof OffscreenCanvas !== 'function') {
      throw new Error('OffscreenCanvas/createImageBitmap unsupported');
    }
    const bitmap = await createImageBitmap(blob as Blob);
    const { w, h } = fitInside(bitmap.width || PREVIEW_MAX, bitmap.height || PREVIEW_MAX);
    const canvas = new OffscreenCanvas(w, h);
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2d context unavailable');
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close();
    const out = await canvas.convertToBlob({ type: 'image/webp', quality: PREVIEW_QUALITY });
    const buf = await out.arrayBuffer();
    // Transfer the underlying buffer (zero-copy) back to the main thread.
    workerScope.postMessage({ id: id as number, ok: true, bytes: new Uint8Array(buf) }, [buf]);
  } catch (err: unknown) {
    const message = (err as { message?: unknown } | null | undefined)?.message;
    workerScope.postMessage(
      {
        id: id as number,
        ok: false,
        error: String(message || err),
      },
      [],
    );
  }
};
