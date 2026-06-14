// Renderer-side prep for manual bookmarks. The browser context has canvas, image,
// video and (via pdfjs) PDF decoding — none of which the Node main process has
// without native deps — so we build every file's *preview* here and hand the main
// process plain bytes to persist. Each file yields:
//   - original bytes (the real file, saved verbatim)
//   - preview bytes (a webp image always renderable in the card / modal):
//       image → downscaled copy · video → first frame · pdf → page 1 · file → icon
// Generic files get a drawn icon+extension thumbnail so they show *something*
// instead of a broken image (the user opted for "pdf preview, files as icon").
// pdfjs (~300 KB gz + a worker chunk) is loaded lazily the first time a PDF
// preview is actually needed, so it never weighs on app startup.
let pdfjsPromise = null;
function loadPdfjs() {
  if (!pdfjsPromise) {
    pdfjsPromise = (async () => {
      const pdfjsLib = await import('pdfjs-dist');
      const workerUrl = (await import('pdfjs-dist/build/pdf.worker.min.mjs?url')).default;
      pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;
      return pdfjsLib;
    })();
  }
  return pdfjsPromise;
}

const PREVIEW_MAX = 768; // longest preview edge, in px (crisp on retina cards)
const PREVIEW_QUALITY = 0.82;

// Hard caps so a stray huge file can't lock the IPC payload / disk. The total
// cap bounds the single `bookmark:add` invoke payload (all originals travel in
// one IPC message); the main process enforces the same limit defensively.
export const MAX_FILE_BYTES = 200 * 1024 * 1024; // 200 MB per file
export const MAX_TOTAL_BYTES = 500 * 1024 * 1024; // 500 MB across all files
export const MAX_FILES = 12;

const IMAGE_EXT = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'svg'];
const VIDEO_EXT = ['mp4', 'mov', 'webm', 'mkv', 'avi', 'm4v'];

export function extOf(name) {
  const m = /\.([a-z0-9]+)$/i.exec((name || '').trim());
  return m ? m[1].toLowerCase() : '';
}

// Classify a File into the four bookmark kinds, preferring the MIME type and
// falling back to the extension (drag-drop sometimes yields an empty type).
export function classifyFile(file) {
  const mime = (file.type || '').toLowerCase();
  const ext = extOf(file.name);
  if (mime.startsWith('image/') || IMAGE_EXT.includes(ext)) return 'image';
  if (mime.startsWith('video/') || VIDEO_EXT.includes(ext)) return 'video';
  if (mime === 'application/pdf' || ext === 'pdf') return 'pdf';
  return 'file';
}

function canvasToWebpBytes(canvas) {
  return new Promise((resolve) => {
    canvas.toBlob(
      (blob) => {
        if (!blob) return resolve(null);
        // `arrayBuffer()` può rifiutare: resta dentro la Promise e degrada a
        // null (→ icona) invece di lasciarla pendente all'infinito.
        blob.arrayBuffer().then(
          (ab) => resolve(new Uint8Array(ab)),
          () => resolve(null),
        );
      },
      'image/webp',
      PREVIEW_QUALITY,
    );
  });
}

// Fit (w,h) inside a PREVIEW_MAX box, never upscaling.
function fitInside(w, h) {
  const scale = Math.min(1, PREVIEW_MAX / Math.max(w, h));
  return { w: Math.max(1, Math.round(w * scale)), h: Math.max(1, Math.round(h * scale)) };
}

// ─── Image preview: Web Worker (OffscreenCanvas) con fallback main-thread ───
// decode + downscale + encode WebP è CPU-bound: su immagini grandi blocca la
// modale. Lo deleghiamo a un worker; si ricade sul main thread per SVG (che
// createImageBitmap rifiuta) o se il worker non è disponibile/fallisce.
let imgWorker = null; // null = non inizializzato, false = non disponibile
let imgWorkerSeq = 0;
const imgWorkerPending = new Map();

function getImageWorker() {
  if (imgWorker === null) {
    if (typeof Worker === 'undefined') {
      imgWorker = false;
      return null;
    }
    try {
      imgWorker = new Worker(new URL('./imagePreview.worker.js', import.meta.url), {
        type: 'module',
      });
      imgWorker.onmessage = (e) => {
        const { id, ok, bytes, error } = e.data || {};
        const cb = imgWorkerPending.get(id);
        if (!cb) return;
        imgWorkerPending.delete(id);
        if (ok) cb.resolve(bytes);
        else cb.reject(new Error(error || 'image worker failed'));
      };
      imgWorker.onerror = () => {
        // Worker morto: rigetta i pending e disabilita → tutti ricadono sul main.
        for (const cb of imgWorkerPending.values()) cb.reject(new Error('image worker crashed'));
        imgWorkerPending.clear();
        imgWorker = false;
      };
    } catch {
      imgWorker = false;
      return null;
    }
  }
  return imgWorker || null;
}

function imagePreviewViaWorker(file) {
  const worker = getImageWorker();
  if (!worker) return Promise.reject(new Error('image worker unavailable'));
  const id = ++imgWorkerSeq;
  return new Promise((resolve, reject) => {
    imgWorkerPending.set(id, { resolve, reject });
    // Il Blob è strutturalmente clonabile e passato per riferimento (no copia dei
    // byte sull'heap JS); il risultato torna come buffer trasferito (zero-copy).
    worker.postMessage({ id, blob: file });
  });
}

// Fallback: stessa resa, ma sul thread UI (gestisce SVG via <Image>).
async function imagePreviewMainThread(file) {
  const url = URL.createObjectURL(file);
  try {
    const img = await new Promise((resolve, reject) => {
      const el = new Image();
      el.onload = () => resolve(el);
      el.onerror = () => reject(new Error('image decode failed'));
      el.src = url;
    });
    const { w, h } = fitInside(img.naturalWidth || PREVIEW_MAX, img.naturalHeight || PREVIEW_MAX);
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    canvas.getContext('2d').drawImage(img, 0, 0, w, h);
    return await canvasToWebpBytes(canvas);
  } finally {
    URL.revokeObjectURL(url);
  }
}

async function imagePreview(file) {
  try {
    const bytes = await imagePreviewViaWorker(file);
    if (bytes) return bytes;
  } catch {
    /* worker non disponibile / SVG / encode fallito → fallback sotto */
  }
  return imagePreviewMainThread(file);
}

async function videoPreview(file) {
  const url = URL.createObjectURL(file);
  const video = document.createElement('video');
  try {
    video.muted = true;
    video.preload = 'auto';
    video.src = url;
    await new Promise((resolve, reject) => {
      const fail = () => reject(new Error('video load failed'));
      video.onloadeddata = () => resolve();
      video.onerror = fail;
      setTimeout(fail, 15000);
    });
    // Seek a touch into the clip so we don't grab a black leading frame.
    const target = Math.min(0.1, (video.duration || 1) / 2);
    await new Promise((resolve) => {
      video.onseeked = () => resolve();
      try {
        video.currentTime = target;
      } catch {
        resolve();
      }
      setTimeout(resolve, 4000);
    });
    const { w, h } = fitInside(video.videoWidth || PREVIEW_MAX, video.videoHeight || PREVIEW_MAX);
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    canvas.getContext('2d').drawImage(video, 0, 0, w, h);
    return await canvasToWebpBytes(canvas);
  } finally {
    URL.revokeObjectURL(url);
    video.removeAttribute('src');
    video.load();
  }
}

async function pdfPreview(buf) {
  const pdfjsLib = await loadPdfjs();
  // La task va creata DENTRO il try: se `task.promise` rifiuta (PDF corrotto o
  // file non-PDF classificato 'pdf' per estensione) il finally deve comunque
  // distruggerla, altrimenti il transport sul worker condiviso resta orfano.
  const task = pdfjsLib.getDocument({ data: new Uint8Array(buf.slice(0)) });
  let pdf;
  try {
    pdf = await task.promise;
    const page = await pdf.getPage(1);
    const base = page.getViewport({ scale: 1 });
    const scale = Math.min(2, PREVIEW_MAX / Math.max(base.width, base.height));
    const viewport = page.getViewport({ scale });
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.ceil(viewport.width));
    canvas.height = Math.max(1, Math.ceil(viewport.height));
    await page.render({ canvasContext: canvas.getContext('2d'), viewport }).promise;
    return await canvasToWebpBytes(canvas);
  } finally {
    pdf?.cleanup?.();
    task.destroy?.();
  }
}

// A drawn placeholder for files we can't render: a document glyph + the uppercase
// extension on the app's dark card background.
async function iconPreview(name) {
  const size = 512;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = '#1a1a1a';
  ctx.fillRect(0, 0, size, size);
  // Folded-corner sheet
  const x = 156;
  const y = 120;
  const w = 200;
  const h = 272;
  const fold = 56;
  ctx.fillStyle = '#2e2e2e';
  ctx.beginPath();
  ctx.moveTo(x, y);
  ctx.lineTo(x + w - fold, y);
  ctx.lineTo(x + w, y + fold);
  ctx.lineTo(x + w, y + h);
  ctx.lineTo(x, y + h);
  ctx.closePath();
  ctx.fill();
  ctx.fillStyle = '#3a3a3a';
  ctx.beginPath();
  ctx.moveTo(x + w - fold, y);
  ctx.lineTo(x + w, y + fold);
  ctx.lineTo(x + w - fold, y + fold);
  ctx.closePath();
  ctx.fill();
  const ext = (extOf(name) || 'file').toUpperCase().slice(0, 5);
  ctx.fillStyle = '#b9a6ff';
  ctx.font = '700 46px ui-sans-serif, system-ui, sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(ext, x + w / 2, y + h / 2 + 18);
  return canvasToWebpBytes(canvas);
}

async function readBytes(file) {
  return new Uint8Array(await file.arrayBuffer());
}

// Turn a File into the wire payload for the `bookmark:add` IPC: original bytes +
// a preview always present (icon as last resort). Never throws on a preview
// failure — it degrades to the icon so the file is still saved.
export async function prepareFile(file) {
  if (file.size > MAX_FILE_BYTES) {
    throw new Error(`"${file.name}" supera il limite di 200 MB.`);
  }
  const kind = classifyFile(file);
  const original = await readBytes(file);
  let preview = null;
  try {
    if (kind === 'image') preview = await imagePreview(file);
    else if (kind === 'video') preview = await videoPreview(file);
    else if (kind === 'pdf') preview = await pdfPreview(original);
  } catch (err) {
    console.warn('[bookmarkFiles] preview failed for', file.name, err?.message);
  }
  if (!preview) preview = await iconPreview(file.name);
  return { name: file.name, mime: file.type || '', kind, original, preview };
}

// Build a transient preview URL for the in-modal thumbnail strip (revoke when done).
export function previewObjectUrl(bytes) {
  if (!bytes) return null;
  return URL.createObjectURL(new Blob([bytes], { type: 'image/webp' }));
}
