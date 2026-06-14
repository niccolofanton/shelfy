'use strict';

// Runs in the webview's MAIN WORLD (injected via executeJavaScript on dom-ready /
// did-finish-load, which bypasses the page CSP). Draws a selection checkbox in the
// bottom-left corner of every saved post's card, with a "Già in database" label
// next to it for posts that already exist in the local DB (those stay disabled —
// they're already imported). Selection is resolved back to full post records via
// window.__ssCapturedItems (populated by webview-injected.js), with a DOM fallback
// so selection still works even when the API response was missed.
//
// Host ↔ overlay protocol:
//   overlay → host  : window.__socialSavedBridge.sendSelect({type, ...}) on the
//                     'ss-select' ipc-message channel (postMessage fallback).
//                       { type:'count', count, platform }    selection size changed
//                       { type:'check', keys:[...], platform } "are these saved?"
//                       (keys are DOM-derived shortcodes on IG, ids on TW/Pinterest)
//   host → overlay  : executeJavaScript on window.__ssSelect.*
//                       enable() / disable()         toggle select mode
//                       markSaved([{key,id}])        flag posts as in-DB (disabled);
//                                                    key = matched shortcode/id, id = DB id
//                       clearSelection()             reset selection
//                       collectJSON() → string       selected posts as JSON

(function () {
  if (window.__ssSelectInjected) return;
  window.__ssSelectInjected = true;

  const ACCENT = '#7B5CFF'; // brand purple — selected state

  const state = {
    enabled: false,
    platform: detectPlatform(),
    selected: new Set(), // post keys (shortcode on IG, tweet id on TW)
    saved: new Set(), // keys known to be in the local DB
    savedId: new Map(), // key → DB id (to open the post modal on the host)
    queried: new Set(), // ids already asked-about, to avoid re-querying the host
    decorated: new Set(), // card elements we've attached an overlay to
    hostByKey: new Map(), // key → card element (for DOM-fallback resolution)
    pos: new Map(), // key → vertical offset within the grid (survives unmount)
    observer: null,
    observerTarget: null, // current MutationObserver target (grid root or body)
    rafPending: false,
    lastFound: -1, // last reported post count (debug, avoids log spam)
  };

  function detectPlatform() {
    const h = location.hostname || '';
    if (h.indexOf('instagram') !== -1) return 'instagram';
    if (h.indexOf('pinterest') !== -1) return 'pinterest';
    if (h.indexOf('x.com') !== -1 || h.indexOf('twitter') !== -1) return 'twitter';
    return 'unknown';
  }

  // True only on a saved-posts GRID page. When the user opens a post, Instagram
  // navigates in-page to /p|reel|tv/{shortcode}/ (the grid stays mounted behind
  // the modal) and the open post's comment thread is full of /p/ /reel/ links —
  // decorating those drops stray checkboxes over the comments. So we decorate
  // only while the URL is the listing itself, and strip the overlay otherwise.
  function onListingPage() {
    const p = location.pathname || '';
    if (state.platform === 'twitter') return /\/bookmarks(\/|$)/.test(p);
    if (state.platform === 'pinterest') {
      // Board page /<user>/<board>/ (and sections); never a pin detail
      // (/pin/<id>/), a reserved root (search/business/…) nor a profile tab
      // (/<user>/_boards|_saved/…). Keep the reserved list in sync with
      // PIN_RESERVED_ROOTS in src/views/Browser.jsx.
      if (
        /^\/(?:pin|search|business|ideas|today|categories|news|settings|notifications|discover|pin-builder)\//.test(
          p,
        )
      )
        return false;
      return /^\/[^/?#]+\/(?!_)[^/?#]+/.test(p);
    }
    // Instagram: /<user>/saved/all-posts/ or /<user>/saved/<folder>/<id>/. A post
    // detail (/p/, /reel/, /tv/) or any profile page must NOT decorate.
    return /\/saved(\/|$)/.test(p);
  }

  function captured() {
    return window.__ssCapturedItems || {};
  }

  function relay(payload) {
    try {
      if (window.__socialSavedBridge && window.__socialSavedBridge.sendSelect) {
        window.__socialSavedBridge.sendSelect(payload);
        return;
      }
    } catch (_) {}
    try {
      window.postMessage({ type: 'SOCIAL_SAVED_SELECT', payload }, location.origin);
    } catch (_) {}
  }

  function emitCount() {
    relay({ type: 'count', count: state.selected.size, platform: state.platform });
  }

  // ── Item resolution ───────────────────────────────────────────────────────────
  // Prefer the rich captured record (full media list, author, etc.); fall back to
  // scraping the bare minimum from the DOM so a missed API response never leaves a
  // checkbox that does nothing.

  function bestSrc(img) {
    if (!img) return '';
    const ss = img.getAttribute('srcset');
    if (ss) {
      const parts = ss.split(',').map((s) => s.trim().split(/\s+/));
      const last = parts[parts.length - 1];
      if (last && last[0]) return last[0];
    }
    return img.currentSrc || img.src || '';
  }

  function igFallback(card, shortcode) {
    const url = bestSrc(card && card.querySelector('img'));
    return {
      id: shortcode,
      platform: 'instagram',
      shortcode,
      postUrl: 'https://www.instagram.com/p/' + shortcode + '/',
      profileUrl: '',
      authorUsername: '',
      authorName: '',
      text: '',
      thumbnailUrl: url,
      mediaType: 'image',
      media: url ? [{ type: 'image', url }] : [],
      timestamp: '',
    };
  }

  function twFallback(card, id) {
    const article = (card.closest && card.closest('article')) || card;
    const imgs = Array.prototype.slice.call(article.querySelectorAll('img[src*="/media/"]'));
    const media = imgs.map((i) => ({ type: 'image', url: bestSrc(i) })).filter((m) => m.url);
    const userA = article.querySelector('a[role="link"][href^="/"]');
    const authorUsername = userA
      ? (userA.getAttribute('href') || '').replace(/^\//, '').split('/')[0]
      : '';
    return {
      id,
      platform: 'twitter',
      shortcode: '',
      postUrl: 'https://x.com/' + (authorUsername || 'i') + '/status/' + id,
      profileUrl: authorUsername ? 'https://x.com/' + authorUsername : '',
      authorUsername,
      authorName: '',
      text: '',
      thumbnailUrl: media[0] ? media[0].url : '',
      mediaType: media.length > 1 ? 'images' : 'image',
      media,
      timestamp: '',
    };
  }

  function pinFallback(card, id) {
    const url = bestSrc(card && card.querySelector('img'));
    return {
      id: id,
      platform: 'pinterest',
      shortcode: '',
      postUrl: 'https://www.pinterest.com/pin/' + id + '/',
      profileUrl: '',
      authorUsername: '',
      authorName: '',
      text: '',
      thumbnailUrl: url,
      mediaType: 'image',
      media: url ? [{ type: 'image', url }] : [],
      timestamp: '',
    };
  }

  function resolveItem(key) {
    const c = captured()[key];
    if (c) return c;
    const host = state.hostByKey.get(key);
    if (!host) return null;
    if (state.platform === 'twitter') return twFallback(host, key);
    if (state.platform === 'pinterest') return pinFallback(host, key);
    return igFallback(host, key);
  }

  // ── Post discovery ──────────────────────────────────────────────────────────
  // Returns [{ card, key }] — `card` is the whole post container we anchor the
  // overlay to (the checkbox sits in its bottom-left corner).

  function findPosts() {
    const out = [];
    if (state.platform === 'twitter') {
      const arts = document.querySelectorAll('article[data-testid="tweet"]');
      for (const art of arts) {
        let id = null;
        const links = art.querySelectorAll('a[href*="/status/"]');
        for (const l of links) {
          const m = (l.getAttribute('href') || '').match(/\/status\/(\d+)/);
          if (m) {
            id = m[1];
            if (l.querySelector('time')) break;
          }
        }
        if (id) out.push({ card: art, key: id });
      }
      return out;
    }
    if (state.platform === 'pinterest') {
      // Pinterest grid tiles are anchors to /pin/{id}/; anchor the overlay to the
      // nearest sized box (pickCard) like Instagram.
      const anchors = document.querySelectorAll('a[href*="/pin/"]');
      const usedCards = new Set();
      for (const a of anchors) {
        const m = (a.getAttribute('href') || '').match(/\/pin\/(\d+)/);
        if (!m) continue;
        const card = pickCard(a);
        if (!card || usedCards.has(card)) continue;
        usedCards.add(card);
        out.push({ card, key: m[1] });
      }
      return out;
    }
    // Instagram (default): grid tiles are anchors to /p|reel|tv/{shortcode}/.
    // Match substrings so both relative and absolute hrefs are caught. The anchor
    // itself may be zero-sized or display:contents (newer IG), so anchor the overlay
    // to the nearest real sizable box (pickCard) instead of requiring the <a> to
    // have a measurable size.
    const anchors = document.querySelectorAll('a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]');
    const usedCards = new Set();
    for (const a of anchors) {
      const m = (a.getAttribute('href') || '').match(/\/(?:p|reel|tv)\/([^/?#]+)/);
      if (!m) continue;
      const card = pickCard(a);
      if (!card || usedCards.has(card)) continue;
      usedCards.add(card);
      out.push({ card, key: m[1] });
    }
    return out;
  }

  // Walk up from an Instagram tile anchor to the nearest real, sized box that wraps
  // THIS post only (display:contents / zero-size anchors can't host the absolutely-
  // positioned overlay). We must not climb so high that one box wraps several tiles
  // — that would collapse a whole row into a single checkbox. Falls back to the anchor.
  function boxOf(el) {
    // Cache the computed `display` per element: it's effectively static for a
    // given tile/anchor, so re-reading it via getComputedStyle on every ancestor
    // on every scan is wasted work. The rect IS re-measured each call (it changes
    // on scroll and is needed fresh by recordPositions).
    let disp = el.__ssDisp;
    if (disp === undefined) {
      try {
        disp = getComputedStyle(el).display;
      } catch (_) {
        disp = '';
      }
      el.__ssDisp = disp;
    }
    const r = el.getBoundingClientRect();
    return { ok: disp !== 'contents' && r.width >= 40 && r.height >= 40, r };
  }
  // Grid-tile selector for the current platform. Instagram/Twitter keep the IG
  // anchor set (unchanged behaviour); Pinterest tiles are /pin/ anchors.
  function postSel() {
    return state.platform === 'pinterest'
      ? 'a[href*="/pin/"]'
      : 'a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]';
  }
  function pickCard(a) {
    if (boxOf(a).ok) return a; // common case: the anchor itself is a real tile
    let el = a.parentElement;
    for (let i = 0; i < 4 && el; i++) {
      // Stop before a container that holds more than one post tile.
      if (el.querySelectorAll(postSel()).length > 1) break;
      if (boxOf(el).ok) return el;
      el = el.parentElement;
    }
    return a;
  }

  // ── Position tracking (defeats virtual scrolling) ─────────────────────────────
  // Instagram/X keep only a window of ~20–45 tiles mounted and UNMOUNT everything
  // off-screen (verified: scrolling away drops the previous tiles from the DOM
  // entirely). So a range computed from the live DOM loses its anchor the moment
  // you scroll past it. Instead we record each tile's vertical offset INSIDE the
  // grid container (invariant to scrolling, since DOM order == visual order) the
  // first time it mounts, and keep it after the tile is unmounted. The range is
  // then resolved against these persisted positions, not the instantaneous DOM.

  // Nearest ancestor that holds every currently-mounted tile (the grid's row
  // container, whose height is reserved for the full list). Used as a stable
  // zero-point so offsets are comparable across scroll states.
  function gridRootEl() {
    const all = document.querySelectorAll(postSel());
    if (!all.length) return document.body;
    let el = all[0];
    while (el && el !== document.body && el.parentElement) {
      if (el.querySelectorAll(postSel()).length >= all.length) break;
      el = el.parentElement;
    }
    return el || document.body;
  }

  function recordPositions(posts) {
    if (!posts || !posts.length) return;
    let base = 0;
    try {
      base = gridRootEl().getBoundingClientRect().top;
    } catch (_) {}
    for (const { card, key } of posts) {
      if (!key || !card) continue;
      try {
        state.pos.set(key, card.getBoundingClientRect().top - base);
      } catch (_) {}
    }
  }

  // ── Overlay rendering ─────────────────────────────────────────────────────────

  const CHECK_SVG =
    '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="#fff" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';

  function decorate(card, key) {
    state.hostByKey.set(key, card);
    if (!card.__ssBox) {
      const cs = getComputedStyle(card);
      if (cs.position === 'static') card.style.position = 'relative';

      const wrap = document.createElement('div');
      wrap.className = 'ss-ov';
      wrap.style.cssText =
        'position:absolute;inset:0;z-index:2147483000;pointer-events:none;border-radius:inherit;' +
        'transition:box-shadow .12s ease;';

      // Bottom-left control bar: checkbox + (optional) "Già in database" label.
      const bar = document.createElement('div');
      bar.style.cssText =
        'position:absolute;bottom:10px;left:10px;display:flex;align-items:center;gap:8px;pointer-events:none;';

      const box = document.createElement('div');
      box.className = 'ss-check';
      box.setAttribute('data-ss-check', '1');
      box.setAttribute('data-ss-key', key);
      box.style.cssText =
        'width:28px;height:28px;border-radius:9px;border:2.5px solid #fff;' +
        'background:rgba(0,0,0,0.55);display:flex;align-items:center;justify-content:center;' +
        'cursor:pointer;pointer-events:auto;flex-shrink:0;' +
        'box-shadow:0 2px 10px rgba(0,0,0,0.65),0 0 0 1px rgba(0,0,0,0.35);' +
        'transition:background .12s ease,border-color .12s ease,transform .08s ease;';

      // "Già in database" — shown only for already-imported posts, beside the box.
      // Clickable: opens the post's modal on the host (tooltip explains it).
      const label = document.createElement('div');
      label.className = 'ss-label';
      label.setAttribute('data-ss-open', '1');
      label.title = 'Premi per vedere il post';
      label.style.cssText =
        'display:none;align-items:center;gap:6px;padding:4px 10px;border-radius:999px;' +
        'font:600 11px/1.4 -apple-system,system-ui,sans-serif;color:#e8e8e8;' +
        'background:rgba(20,20,20,0.85);border:1px solid rgba(255,255,255,0.22);' +
        'box-shadow:0 2px 8px rgba(0,0,0,0.55);pointer-events:auto;cursor:pointer;white-space:nowrap;' +
        'transition:background .12s ease,border-color .12s ease;';
      label.innerHTML =
        '<span style="width:6px;height:6px;border-radius:50%;background:#9aa0a6;flex-shrink:0;"></span>' +
        '<span>Già in database</span>' +
        '<svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="#b8b8b8" stroke-width="2" ' +
        'stroke-linecap="round" stroke-linejoin="round" style="flex-shrink:0;"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>';
      label.addEventListener('mouseenter', () => {
        label.style.background = 'rgba(45,45,45,0.92)';
        label.style.borderColor = 'rgba(255,255,255,0.4)';
      });
      label.addEventListener('mouseleave', () => {
        label.style.background = 'rgba(20,20,20,0.85)';
        label.style.borderColor = 'rgba(255,255,255,0.22)';
      });

      bar.appendChild(box);
      bar.appendChild(label);
      // Darkening gradient toward the bottom (where the checkbox + "Già in database"
      // badge sit) so they stay legible over bright thumbnails. Instagram only —
      // Twitter cards are large text containers where a tile gradient would look wrong.
      if (state.platform === 'instagram' || state.platform === 'pinterest') {
        const grad = document.createElement('div');
        grad.className = 'ss-grad';
        grad.style.cssText =
          'position:absolute;inset:0;pointer-events:none;border-radius:inherit;' +
          'background:linear-gradient(to top, rgba(0,0,0,0.74) 0%, rgba(0,0,0,0.46) 16%, rgba(0,0,0,0.14) 36%, rgba(0,0,0,0) 58%);';
        wrap.appendChild(grad);
      }
      wrap.appendChild(bar);
      card.appendChild(wrap);
      card.__ssWrap = wrap;
      card.__ssBox = box;
      card.__ssLabel = label;
      state.decorated.add(card);
    }
    card.__ssKey = key;
    update(card, key);
  }

  function update(card, key) {
    const box = card.__ssBox;
    const label = card.__ssLabel;
    const wrap = card.__ssWrap;
    if (!box) return;
    const sav = state.saved.has(key);
    const sel = !sav && state.selected.has(key);

    label.style.display = sav ? 'flex' : 'none';
    if (sav) {
      const sid = state.savedId.get(key);
      if (sid) label.setAttribute('data-ss-id', sid);
    }

    if (sav) {
      // Already in the DB → disabled, greyed-out, not selectable.
      box.innerHTML = '';
      box.style.background = 'rgba(60,60,60,0.6)';
      box.style.borderColor = '#9aa0a6';
      box.style.opacity = '0.75';
      box.style.cursor = 'not-allowed';
      box.style.boxShadow = '0 1px 4px rgba(0,0,0,0.4)';
      box.title = 'Già presente nel database';
      if (wrap) wrap.style.boxShadow = 'none';
      return;
    }

    box.style.opacity = '1';
    box.style.cursor = 'pointer';
    box.title = '';
    box.innerHTML = sel ? CHECK_SVG : '';
    box.style.background = sel ? ACCENT : 'rgba(0,0,0,0.55)';
    box.style.borderColor = '#fff';
    box.style.boxShadow = sel
      ? '0 2px 12px rgba(123,92,255,0.7),0 0 0 1px rgba(0,0,0,0.35)'
      : '0 2px 10px rgba(0,0,0,0.65),0 0 0 1px rgba(0,0,0,0.35)';
    if (wrap) wrap.style.boxShadow = sel ? 'inset 0 0 0 3px ' + ACCENT : 'none';
  }

  function refreshAll() {
    for (const card of state.decorated) {
      if (card.isConnected && card.__ssBox) update(card, card.__ssKey);
    }
  }

  // ── Selection ────────────────────────────────────────────────────────────────

  function toggle(key) {
    if (!key || state.saved.has(key)) return; // saved posts are disabled
    if (state.selected.has(key)) state.selected.delete(key);
    else state.selected.add(key);
  }

  // Add every selectable post between the shift-anchor (state.lastKey) and `key`,
  // inclusive, in visual order. Skips already-saved (disabled) posts. Mirrors the
  // gallery's shift-click range select.
  //
  // The order is taken from state.pos (every tile's grid offset, persisted across
  // unmount) rather than the live DOM, so a virtualized anchor that has scrolled
  // out of view is still indexable. Tiles never mounted this session can't be in
  // the range — but normal scrolling between the two ends mounts them on the way.
  function selectRangeTo(key) {
    recordPositions(findPosts()); // refresh offsets of what's mounted right now
    let order = Array.from(state.pos.keys()).sort((a, b) => state.pos.get(a) - state.pos.get(b));
    let from = order.indexOf(state.lastKey);
    let to = order.indexOf(key);
    if (from === -1 || to === -1) {
      // Last resort: whatever is in the DOM this instant.
      order = findPosts().map((p) => p.key);
      from = order.indexOf(state.lastKey);
      to = order.indexOf(key);
    }
    if (from === -1 || to === -1) {
      state.selected.add(key);
      return;
    }
    for (let i = Math.min(from, to); i <= Math.max(from, to); i++) {
      const k = order[i];
      if (k && !state.saved.has(k)) state.selected.add(k);
    }
  }

  // Checkbox activation: a plain click toggles one post; shift+click selects the
  // whole range from the last-clicked checkbox to this one (standard shift-click).
  function activate(key, withShift) {
    if (!key || state.saved.has(key)) return;
    if (withShift && state.lastKey && state.lastKey !== key) {
      selectRangeTo(key);
    } else {
      toggle(key);
    }
    state.lastKey = key;
    refreshAll();
    emitCount();
  }

  // Document-level capture handlers: intercept clicks/taps on our checkbox BEFORE
  // the page (e.g. Instagram's link navigation) can act on them. This is what makes
  // the checkbox reliably clickable over an <a> grid tile.
  function onCapture(e) {
    const t = e.target;
    if (!t || !t.closest) return;
    // "Già in database" label → ask the host to open the post's modal.
    const open = t.closest('[data-ss-open="1"]');
    if (open) {
      e.preventDefault();
      e.stopPropagation();
      if (e.stopImmediatePropagation) e.stopImmediatePropagation();
      if (e.type === 'click') {
        const id = open.getAttribute('data-ss-id');
        if (id) relay({ type: 'open', id: id, platform: state.platform });
      }
      return;
    }
    const box = t.closest('[data-ss-check="1"]');
    if (!box) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.stopImmediatePropagation) e.stopImmediatePropagation();
    if (e.type === 'click') activate(box.getAttribute('data-ss-key'), e.shiftKey);
  }
  const CAPTURE_EVENTS = [
    'click',
    'mousedown',
    'mouseup',
    'pointerdown',
    'pointerup',
    'touchstart',
  ];

  // ── Scan loop ────────────────────────────────────────────────────────────────

  // Strip every overlay we drew but keep selection/saved/pos state, so leaving the
  // grid (e.g. opening a post) and coming back re-draws the same selection.
  function undecorateAll() {
    for (const card of state.decorated) {
      if (card.__ssWrap) card.__ssWrap.remove();
      delete card.__ssWrap;
      delete card.__ssBox;
      delete card.__ssLabel;
      delete card.__ssKey;
    }
    state.decorated.clear();
    state.lastFound = -1;
  }

  function scan() {
    if (!state.enabled) return;
    // Off the listing grid (post detail, profile, …) → no checkboxes at all.
    if (!onListingPage()) {
      if (state.decorated.size) undecorateAll();
      return;
    }
    const posts = findPosts();
    // Re-target the observer onto the (now mounted) grid root if it changed; at
    // enable() time the grid may not exist yet so it defaulted to document.body.
    observeGrid();
    recordPositions(posts); // remember each tile's grid offset before it can unmount
    if (posts.length !== state.lastFound) {
      state.lastFound = posts.length;
      // Diagnostics: only relayed when explicitly enabled (window.__ssDebug),
      // so normal cherry-pick scrolling doesn't spam the host console.
      if (window.__ssDebug) relay({ type: 'debug', platform: state.platform, found: posts.length });
    }
    const toCheck = [];
    for (const { card, key } of posts) {
      decorate(card, key);
      if (state.saved.has(key)) continue;
      // Check by post KEY (shortcode on IG, tweet id on TW) — always derivable from
      // the DOM — so posts saved in earlier sessions are recognised even when their
      // API response wasn't re-intercepted this session.
      if (!state.queried.has(key)) {
        state.queried.add(key);
        toCheck.push(key);
      }
    }
    if (toCheck.length) relay({ type: 'check', keys: toCheck, platform: state.platform });
  }

  function scheduleScan() {
    if (state.rafPending) return;
    state.rafPending = true;
    requestAnimationFrame(() => {
      state.rafPending = false;
      scan();
    });
  }

  // ── Public API (host-driven) ───────────────────────────────────────────────────

  function enable() {
    if (state.enabled) return;
    state.enabled = true;
    for (const evt of CAPTURE_EVENTS) document.addEventListener(evt, onCapture, true);
    scan();
    // Observe the grid root (not the whole document.body subtree) so the scan is
    // only re-scheduled by grid mutations, not by every unrelated image-load /
    // lazy-mount elsewhere on the page. Fall back to document.body when the grid
    // root can't be resolved yet (empty page); re-observe on every scan so we
    // re-target as soon as the grid mounts.
    observeGrid();
    // Low-frequency safety net: a MutationObserver bound to a grid root that an SPA
    // later REPLACES (e.g. switching saved folders/boards) goes deaf on the detached
    // node. Periodically re-target the observer and re-scan so the overlay recovers
    // without the churn of the old 1s forced re-scan.
    state.rescanTimer = setInterval(() => {
      observeGrid();
      scheduleScan();
    }, 2000);
    emitCount();
  }

  // (Re)point the MutationObserver at the current grid root. Cheap to call from
  // scan() — only rebinds when the target element actually changed.
  function observeGrid() {
    if (!state.enabled) return;
    const root = gridRootEl() || document.body;
    if (root === state.observerTarget && state.observer) return;
    if (state.observer) state.observer.disconnect();
    state.observer = new MutationObserver(scheduleScan);
    state.observer.observe(root, { childList: true, subtree: true });
    state.observerTarget = root;
  }

  function disable() {
    state.enabled = false;
    for (const evt of CAPTURE_EVENTS) document.removeEventListener(evt, onCapture, true);
    if (state.rescanTimer) {
      clearInterval(state.rescanTimer);
      state.rescanTimer = null;
    }
    if (state.observer) {
      state.observer.disconnect();
      state.observer = null;
      state.observerTarget = null;
    }
    undecorateAll();
    state.hostByKey.clear();
    state.pos.clear();
    state.selected.clear();
    state.lastKey = null; // drop the shift-click anchor
    emitCount();
  }

  // Note: state.saved / state.savedId persist across disable()/enable() within the
  // same page so re-opening select mode doesn't re-query everything.

  // Flag posts as already in the local library → disabled + label. Accepts
  // [{ key, id }] pairs: `key` is the DOM-derived shortcode/tweet-id we matched on,
  // `id` is the DB primary key (used to open the post's modal on the host).
  function markSaved(pairs) {
    if (!Array.isArray(pairs)) return;
    const decoratedKeys = new Set();
    for (const c of state.decorated) decoratedKeys.add(c.__ssKey);
    let matchedDecor = 0;
    for (const p of pairs) {
      if (!p || !p.key) continue;
      const key = String(p.key);
      state.saved.add(key);
      if (decoratedKeys.has(key)) matchedDecor++;
      if (p.id != null) state.savedId.set(key, String(p.id));
      state.selected.delete(key);
    }
    if (window.__ssDebug)
      relay({
        type: 'debug',
        platform: state.platform,
        marked: pairs.length,
        matchedDecor: matchedDecor,
        sampleDecorKey: decoratedKeys.size ? Array.from(decoratedKeys)[0] : null,
      });
    refreshAll();
    emitCount();
  }

  function clearSelection() {
    state.selected.clear();
    state.lastKey = null; // drop the shift-click anchor
    refreshAll();
    emitCount();
  }

  // Full records for the current selection, as a JSON string (executeJavaScript
  // can only marshal primitives back to the host).
  function collectJSON() {
    const out = [];
    for (const key of state.selected) {
      const item = resolveItem(key);
      if (item) out.push(item);
    }
    return JSON.stringify(out);
  }

  window.__ssSelect = { enable, disable, markSaved, clearSelection, collectJSON, refresh: scan };
})();
