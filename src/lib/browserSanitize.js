// ── Intercept payload validation (cross-world trust boundary) ─────────────────
// Intercepted items arrive from the webview MAIN world, which is fully
// page-controlled: a compromised/hostile IG/X/Pinterest page (or XSS on those
// origins) can call __socialSavedBridge.send with arbitrary payloads. Before the
// renderer forwards them to db:bulkUpsert (which INSERTs them) we clamp the batch
// size, require a bounded non-empty string id, coerce/cap the textual fields, and
// drop media entries whose url isn't http(s). This bounds DB pollution and keeps
// attacker-chosen blobs out of the store; downstream SSRF on media URLs is still
// independently guarded by the downloader.
export const MAX_BATCH_ITEMS = 1000; // hard cap on items accepted per intercepted batch
export const MAX_ID_LEN = 256;
export const MAX_TEXT_LEN = 20000;
export const MAX_URL_LEN = 4096;
export const MAX_MEDIA = 60; // per-post media entries
export const STR_FIELDS = [
  'shortcode',
  'postUrl',
  'profileUrl',
  'authorUsername',
  'authorName',
  'mediaType',
  'timestamp',
];

function clampStr(v, max) {
  if (typeof v !== 'string') return '';
  const out = v.slice(0, max);
  // Avoid leaving a lone high surrogate when the cut lands mid surrogate pair
  // (es. emoji nelle caption IG/TikTok/X): produrrebbe UTF-16 mal formato.
  return /[\uD800-\uDBFF]$/.test(out) ? out.slice(0, -1) : out;
}

function isHttpUrl(u) {
  if (typeof u !== 'string' || u.length > MAX_URL_LEN) return false;
  try {
    const p = new URL(u).protocol;
    return p === 'http:' || p === 'https:';
  } catch {
    return false;
  }
}

export function sanitizeInterceptedItem(it, platform) {
  if (!it || typeof it !== 'object') return null;
  const id = typeof it.id === 'string' ? it.id : it.id != null ? String(it.id) : '';
  if (!id || id.length > MAX_ID_LEN) return null;
  // Force the platform to the batch's validated one: db.bulkUpsert persists the
  // per-item platform, so honouring a page-supplied `it.platform` would let a
  // hostile page label items as e.g. 'web'/'manual' and poison stats + filters.
  const out = { id, platform };
  for (const f of STR_FIELDS) out[f] = clampStr(it[f], f === 'timestamp' ? 64 : MAX_URL_LEN);
  out.text = clampStr(it.text, MAX_TEXT_LEN);
  out.thumbnailUrl = isHttpUrl(it.thumbnailUrl) ? it.thumbnailUrl : '';
  const media = Array.isArray(it.media) ? it.media : [];
  out.media = media
    .filter((m) => m && typeof m === 'object' && isHttpUrl(m.url))
    .slice(0, MAX_MEDIA)
    .map((m) => ({ type: m.type === 'video' ? 'video' : 'image', url: m.url }));
  return out;
}

// Validate + clamp a whole intercepted batch. Returns a new bounded array of
// sanitized items (drops anything without a usable id). `platform` MUST be the
// already-validated batch/tab platform — it is stamped onto every item,
// overriding whatever the page-controlled payload declared.
export function sanitizeInterceptedBatch(items, platform) {
  const out = [];
  for (const it of items) {
    if (out.length >= MAX_BATCH_ITEMS) break;
    const clean = sanitizeInterceptedItem(it, platform);
    if (clean) out.push(clean);
  }
  return out;
}
