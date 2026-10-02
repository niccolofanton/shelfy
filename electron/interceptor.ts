import { session } from 'electron';

const PARTITION = 'persist:social';

// Generic browser User-Agent shared by the social webview and public media
// requests. Download requests never reuse the webview's cookie jar.
const SOCIAL_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

// Permissions explicitly granted to the social webview. Everything else is
// denied. IG/X may probe several capabilities; only allow what is harmless and
// avoids breaking page load. We do NOT need camera/mic/geolocation/midi/etc.
const ALLOWED_PERMISSIONS = new Set<string>(['notifications', 'clipboard-sanitized-write']);

function isAllowed(permission: string): boolean {
  return ALLOWED_PERMISSIONS.has(permission);
}

function setupInterceptor(): void {
  const ses = session.fromPartition(PARTITION);

  // Use a consistent browser User-Agent for the social webview.
  ses.setUserAgent(SOCIAL_UA);

  // Allow-list: grant only the permissions above, deny the rest. Denying does
  // not block page loading for IG/X (those features degrade gracefully).
  ses.setPermissionRequestHandler((_webContents, permission, callback) => {
    callback(isAllowed(permission));
  });
  ses.setPermissionCheckHandler((_webContents, permission) => isAllowed(permission));
}

export { setupInterceptor, PARTITION, SOCIAL_UA };
