// MAIN-world content script (manifest content_scripts[0]: world "MAIN", run_at
// "document_start", top frame), so it patches fetch/XHR before any page script runs.
//
// 1. electron/webview-injected.ts is the desktop capture hook, bundled unchanged. With no
//    contextBridge here, its relay takes the existing postMessage fallback
//    ({type: 'SOCIAL_SAVED_INTERCEPT', items, hasNextPage, platform}) that bridge.ts receives.
// 2. The census wraps fetch/XHR outside the hook's patches (counting only).
// 3. Passive helpers call the hook's Pinterest SSR reader and X DOM scan from page events.

import '../../electron/webview-injected';
import { installCensus } from './main/census';
import { installPassiveHelpers } from './main/passive';

installCensus(window);
installPassiveHelpers(window);
