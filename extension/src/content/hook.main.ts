// MAIN-world content script (manifest content_scripts[0]: world "MAIN", run_at
// "document_start", top frame), so it patches fetch/XHR before any page script runs.
//
// 1. electron/webview-injected.ts is the desktop capture hook, bundled unchanged. With no
//    contextBridge here, its relay takes the existing postMessage fallback
//    ({type: 'SOCIAL_SAVED_INTERCEPT', items, hasNextPage, platform}) that bridge.ts receives.
// 2. Passive helpers call the hook's Pinterest SSR reader and X DOM scan from page events.
// Debug builds bundle hook.debug.ts instead, which adds the request census.

import '../../../electron/webview-injected';
import { installPassiveHelpers } from '../main/passive';

installPassiveHelpers(window);
