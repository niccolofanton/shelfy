// Image preview downscaling off the UI thread. decode (createImageBitmap) +
// drawImage + WebP encode (OffscreenCanvas.convertToBlob) are CPU-bound and, on
// multi-MB images, freeze the modal while importing manual bookmarks. Running them
// here keeps the UI responsive. bookmarkFiles.js falls back to the main-thread
// path (which uses <Image>, so it also covers SVG that createImageBitmap rejects).

// Tenere in sync con bookmarkFiles.js (stessa resa visiva della preview).
const PREVIEW_MAX = 768; // longest preview edge, in px
const PREVIEW_QUALITY = 0.82;

// Fit (w,h) inside a PREVIEW_MAX box, never upscaling.
function fitInside(w, h) {
  const scale = Math.min(1, PREVIEW_MAX / Math.max(w, h));
  return { w: Math.max(1, Math.round(w * scale)), h: Math.max(1, Math.round(h * scale)) };
}

self.onmessage = async (e) => {
  const { id, blob } = e.data || {};
  try {
    if (typeof createImageBitmap !== 'function' || typeof OffscreenCanvas !== 'function') {
      throw new Error('OffscreenCanvas/createImageBitmap unsupported');
    }
    const bitmap = await createImageBitmap(blob);
    const { w, h } = fitInside(bitmap.width || PREVIEW_MAX, bitmap.height || PREVIEW_MAX);
    const canvas = new OffscreenCanvas(w, h);
    const ctx = canvas.getContext('2d');
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close();
    const out = await canvas.convertToBlob({ type: 'image/webp', quality: PREVIEW_QUALITY });
    const buf = await out.arrayBuffer();
    // Transfer the underlying buffer (zero-copy) back to the main thread.
    self.postMessage({ id, ok: true, bytes: new Uint8Array(buf) }, [buf]);
  } catch (err) {
    self.postMessage({ id, ok: false, error: String((err && err.message) || err) });
  }
};
