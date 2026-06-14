const { session } = require('electron');

const PARTITION = 'persist:social';

// Single source of truth for the browser identity we present to IG/X. The
// webview logs in with this UA, so the downloader (fetch + yt-dlp) must reuse
// the exact same string: a cookie jar minted under one UA but replayed under a
// different one is a classic bot signal and a ban risk. Bump it here only.
const SOCIAL_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

// Permissions explicitly granted to the social webview. Everything else is
// denied. IG/X may probe several capabilities; only allow what is harmless and
// avoids breaking page load. We do NOT need camera/mic/geolocation/midi/etc.
const ALLOWED_PERMISSIONS = new Set(['notifications', 'clipboard-sanitized-write']);

function isAllowed(permission) {
  return ALLOWED_PERMISSIONS.has(permission);
}

function setupInterceptor() {
  const ses = session.fromPartition(PARTITION);

  // Set a realistic browser User-Agent to avoid bot detection (shared with the
  // downloader so cookies and requests carry a consistent identity).
  ses.setUserAgent(SOCIAL_UA);

  // Allow-list: grant only the permissions above, deny the rest. Denying does
  // not block page loading for IG/X (those features degrade gracefully).
  ses.setPermissionRequestHandler((_webContents, permission, callback) => {
    callback(isAllowed(permission));
  });
  ses.setPermissionCheckHandler((_webContents, permission) => isAllowed(permission));
}

module.exports = { setupInterceptor, PARTITION, SOCIAL_UA };
