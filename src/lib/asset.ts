// Local assets live in userData and are served through the custom `asset://`
// protocol registered in the main process — file:// is blocked from the dev
// http origin. Keep this in sync with the protocol handler in electron/main.js.
export function assetUrl(filePath: string | null | undefined): string | null {
  return filePath ? `asset://media/${encodeURIComponent(filePath)}` : null;
}

// Downscaled variant for grid tiles: `?w=N` makes the asset protocol serve a
// cached thumbnail instead of the original (often a multi-MB CDN image whose
// decode stutters the gallery). Width is clamped main-side; non-image files
// ignore the param and serve the original bytes.
export function assetThumbUrl(filePath: string | null | undefined, w = 480): string | null {
  return filePath ? `${assetUrl(filePath)}?w=${w}` : null;
}

// True when `url` points at a locally downloaded file served through the
// `asset://` protocol (vs. a remote http/https URL).
export function isAssetUrl(url: unknown): url is string {
  return typeof url === 'string' && url.startsWith('asset://');
}
