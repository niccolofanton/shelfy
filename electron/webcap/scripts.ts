// In-page routines for the web-reference capture (v2). Every export is a JS
// SOURCE STRING evaluated inside the UNTRUSTED captured page (Playwright
// page.evaluate / CDP Runtime.evaluate). They only read or restyle the DOM and
// return plain JSON — no bridge into the app is ever exposed to the page.
//
// Conventions: each probe is an IIFE that never throws (returns a fallback),
// caps its own work on huge DOMs, and reports CSS pixels in document coordinates
// (scroll 0) unless stated otherwise.

// ─── Init script (document start, every frame) ───────────────────────────────
// Records, without changing behaviour:
//  • IntersectionObserver instances and their targets, so the capture can later
//    force every target "in view" (reveal mode) instead of relying on scrolling;
//  • which canvas context types the page itself requests (WebGL presence) — the
//    old probe created contexts and could break canvases the site wanted as 2D.
export const INIT_SCRIPT = `(() => {
  try {
    if (window.__shelfy) return;
    const S = (window.__shelfy = { io: [], ctx: {} });
    const NativeIO = window.IntersectionObserver;
    if (typeof NativeIO === 'function') {
      const Wrapped = function IntersectionObserver(cb, opts) {
        const io = new NativeIO(cb, opts);
        const rec = { io, cb, targets: new Set() };
        if (S.io.length < 2000) S.io.push(rec);
        const obs = io.observe.bind(io), unobs = io.unobserve.bind(io), disc = io.disconnect.bind(io);
        io.observe = (t) => { try { rec.targets.add(t); } catch (e) {} return obs(t); };
        io.unobserve = (t) => { try { rec.targets.delete(t); } catch (e) {} return unobs(t); };
        io.disconnect = () => { try { rec.targets.clear(); } catch (e) {} return disc(); };
        return io;
      };
      Wrapped.prototype = NativeIO.prototype;
      try { Object.defineProperty(Wrapped, 'name', { value: 'IntersectionObserver' }); } catch (e) {}
      window.IntersectionObserver = Wrapped;
    }
    const proto = window.HTMLCanvasElement && HTMLCanvasElement.prototype;
    if (proto && proto.getContext) {
      const orig = proto.getContext;
      proto.getContext = function (type) {
        const ctx = orig.apply(this, arguments);
        try {
          if (ctx) {
            const k = String(type || '').toLowerCase().replace('experimental-', '');
            S.ctx[k] = (S.ctx[k] || 0) + 1;
          }
        } catch (e) {}
        return ctx;
      };
    }
  } catch (e) {}
})();`;

// ─── Blocked / challenge / login-wall detection ──────────────────────────────
// Returns { blocked, vendor, reason }. Conservative: a CAPTCHA widget inside an
// otherwise normal page (newsletter form) does NOT count — only interstitials
// with little content of their own.
export const JS_DETECT_BLOCKED = `(() => {
  try {
    const title = (document.title || '').toLowerCase();
    const html = document.documentElement ? document.documentElement.outerHTML.slice(0, 300000).toLowerCase() : '';
    const body = document.body;
    const text = body ? (body.innerText || '').slice(0, 6000).toLowerCase() : '';
    const textLen = body ? (body.innerText || '').trim().length : 0;
    const links = document.querySelectorAll('a[href]').length;
    const thin = textLen < 1500 && links < 15;
    const has = (sel) => { try { return !!document.querySelector(sel); } catch (e) { return false; } };
    const out = (vendor, reason) => ({ blocked: true, vendor, reason });
    if (/just a moment|attention required|un momento|un instant|einen moment|un momentito|security checkpoint/.test(title) &&
        /cloudflare|cf-chl|challenge-platform|cf_chl_opt|vercel/.test(html)) {
      return out(/vercel/.test(html) ? 'vercel' : 'cloudflare', 'challenge title');
    }
    if (thin && (has('#challenge-form') || has('#challenge-running') || has('#challenge-stage') ||
        has('#cf-challenge-running') || has('.cf-browser-verification') ||
        /cdn-cgi\\/challenge-platform|cf_chl_opt|cf-turnstile/.test(html))) {
      return out('cloudflare', 'challenge page');
    }
    if (thin && /performing security verification|verifying you are human|verify you are human|checking your browser|checking if the site connection is secure/.test(text)) {
      return out('cloudflare', 'verification text');
    }
    if (thin && (has('iframe[src*="captcha-delivery.com"]') || /geo\\.captcha-delivery\\.com|datadome/.test(html))) {
      return out('datadome', 'captcha-delivery');
    }
    if (thin && (has('#px-captcha') || /press (&|and) hold|px-captcha|perimeterx|human-challenge/.test(html + ' ' + text))) {
      return out('perimeterx', 'press and hold');
    }
    if (/access denied/.test(title) && /reference #|edgesuite|akamai/.test(html + ' ' + text)) {
      return out('akamai', 'access denied');
    }
    if (thin && /incapsula incident id|_incapsula_resource|request unsuccessful/.test(html + ' ' + text)) {
      return out('imperva', 'incapsula');
    }
    if (/sucuri website firewall/.test(title + ' ' + text)) return out('sucuri', 'firewall');
    if (thin && /awswaf|aws-waf-token|gokuprops/.test(html)) return out('aws-waf', 'captcha');
    if (thin && (has('iframe[src*="recaptcha"]') || has('iframe[src*="hcaptcha"]')) &&
        /robot|captcha|verify|verifica|human|umano/.test(text + ' ' + title)) {
      return out('captcha', 'full-page captcha');
    }
    // Login wall: a visible password field and almost nothing else on the page.
    if (textLen < 500 && links < 12) {
      for (const el of document.querySelectorAll('input[type="password"]')) {
        const r = el.getBoundingClientRect();
        if (r.width > 0 && r.height > 0) return out('login', 'login wall');
      }
    }
    return { blocked: false };
  } catch (e) { return { blocked: false }; }
})()`;

// ─── Readiness ────────────────────────────────────────────────────────────────
// True while a full-screen loader/preloader/intro overlay actually covers the
// viewport (intersects ≥60% AND is the element hit at the centre). A curtain
// moved off-screen with transform or clip-path no longer counts.
export const JS_LOADER_VISIBLE = `(() => {
  try {
    const vw = innerWidth, vh = innerHeight;
    const sel = '[class*=preload i],[id*=preload i],[class*=loader i],[id*=loader i],[class*=loading i],[id*=loading i],[class*=splash i],[id*=splash i],[class*=intro i],[id*=intro i]';
    const hit = document.elementFromPoint(vw / 2, vh / 2);
    for (const el of document.querySelectorAll(sel)) {
      const s = getComputedStyle(el);
      if (s.display === 'none' || s.visibility === 'hidden' || parseFloat(s.opacity || '1') < 0.05) continue;
      if (s.position !== 'fixed' && s.position !== 'absolute') continue;
      const r = el.getBoundingClientRect();
      const ix = Math.max(0, Math.min(r.right, vw) - Math.max(r.left, 0));
      const iy = Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0));
      if (ix * iy < vw * vh * 0.6) continue;
      if (hit && (el === hit || el.contains(hit))) return true;
    }
    return false;
  } catch (e) { return false; }
})()`;

// Visible text characters and decoded images inside the first viewport — a
// cheap "did the page paint anything real" signal for QC.
export const JS_VIEWPORT_CONTENT = `(() => {
  try {
    const vw = innerWidth, vh = innerHeight;
    let chars = 0, images = 0, media = 0;
    const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    let n, guard = 0;
    while ((n = walker.nextNode()) && guard++ < 20000) {
      const t = n.nodeValue; if (!t || !t.trim()) continue;
      const el = n.parentElement; if (!el) continue;
      const r = el.getBoundingClientRect();
      if (r.bottom < 0 || r.top > vh || r.right < 0 || r.left > vw || r.width === 0) continue;
      const s = getComputedStyle(el);
      if (s.visibility === 'hidden' || parseFloat(s.opacity || '1') < 0.1) continue;
      chars += t.trim().length;
      if (chars > 4000) break;
    }
    for (const im of document.images) {
      const r = im.getBoundingClientRect();
      if (r.bottom < 0 || r.top > vh || r.width < 40 || r.height < 40) continue;
      if (im.complete && im.naturalWidth > 0) images++;
    }
    for (const v of document.querySelectorAll('video, canvas')) {
      const r = v.getBoundingClientRect();
      if (r.bottom < 0 || r.top > vh || r.width < 100 || r.height < 100) continue;
      media++;
    }
    return { chars, images, media };
  } catch (e) { return { chars: 0, images: 0, media: 0 }; }
})()`;

// Wait (≤ maxMs) for web fonts and the images inside the first viewport.
export function jsWaitAssets(maxMs: number): string {
  return `(async () => {
    const deadline = Date.now() + ${Math.max(0, Math.floor(maxMs))};
    const left = () => Math.max(0, deadline - Date.now());
    const race = (p) => Promise.race([p, new Promise((r) => setTimeout(r, left()))]);
    try { if (document.fonts && document.fonts.ready) await race(document.fonts.ready); } catch (e) {}
    try {
      const vh = innerHeight * 1.2;
      const imgs = Array.from(document.images).filter((im) => {
        const r = im.getBoundingClientRect();
        return r.top < vh && r.bottom > 0 && r.width > 0;
      });
      await race(Promise.all(imgs.map((im) => (im.complete ? null : im.decode ? im.decode().catch(() => {}) : null))));
    } catch (e) {}
    return true;
  })()`;
}

// ─── Pop-ups that survive the consent engine and the cosmetic filters ────────
// Removes modal dialogs/backdrops (newsletter, age gates already accepted, promo
// overlays) and floating chat launchers. Never touches the site header, the hero,
// or anything that does not look like an overlay. Returns how many were removed.
export const JS_REMOVE_OVERLAYS = `(() => {
  let removed = 0;
  try {
    const vw = innerWidth, vh = innerHeight;
    const kill = (el) => { try { el.setAttribute('data-shelfy-removed', '1'); el.style.setProperty('display', 'none', 'important'); removed++; } catch (e) {} };
    const KW = /newsletter|subscribe|iscriviti|sign up|signup|popup|pop-up|modal|promo|discount|sconto|offer|offerta|coupon|cookie|consent|gdpr|privacy|age-gate|agegate|chat|intercom|drift|crisp|tidio|zendesk|livechat|hubspot|klaviyo|accessib|userway|acsb|equalweb|audioeye|a11y/i;
    const nodes = document.querySelectorAll('[role="dialog"],[aria-modal="true"],dialog[open],div,section,aside,iframe');
    let guard = 0;
    for (const el of nodes) {
      if (guard++ > 6000) break;
      if (el.closest('[data-shelfy-removed]')) continue;
      const s = getComputedStyle(el);
      if (s.position !== 'fixed' && s.position !== 'sticky' && !(el.matches('dialog[open]'))) continue;
      if (s.display === 'none' || s.visibility === 'hidden') continue;
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      const area = r.width * r.height;
      const isDialog = el.matches('[role="dialog"],[aria-modal="true"],dialog[open]');
      const label = ((el.id || '') + ' ' + (typeof el.className === 'string' ? el.className : '') + ' ' + (el.getAttribute('aria-label') || '') + ' ' + (el.getAttribute('title') || '') + ' ' + (el.tagName === 'IFRAME' ? (el.getAttribute('src') || '') : '')).slice(0, 400);
      const text = (el.innerText || '').slice(0, 600);
      const bg = s.backgroundColor;
      const alpha = (() => { const m = /rgba?\\(([^)]+)\\)/.exec(bg || ''); if (!m) return 1; const p = m[1].split(','); return p.length > 3 ? parseFloat(p[3]) : 1; })();
      // 1) Explicit dialogs with promo/consent wording.
      if (isDialog && (KW.test(label) || KW.test(text))) { kill(el); continue; }
      // 2) Semi-transparent full-viewport backdrops (behind a modal).
      if (area >= vw * vh * 0.85 && alpha > 0.05 && alpha < 0.95 && !(el.querySelector('nav, header, h1, canvas, video'))) { kill(el); continue; }
      // 3) Floating chat launchers / widgets: small, pinned to a bottom corner.
      const corner = r.bottom >= vh - 8 - 40 && (r.right >= vw - 40 || r.left <= 40);
      if (s.position === 'fixed' && corner && r.width < 460 && r.height < 720 && KW.test(label)) { kill(el); continue; }
      // 4) Bottom/top banners about cookies or newsletters that span the width.
      if (s.position === 'fixed' && r.width >= vw * 0.6 && r.height < vh * 0.45 && /cookie|consent|gdpr|newsletter|subscribe|iscriviti/i.test(label + ' ' + text)) { kill(el); continue; }
    }
    // Unlock vertical scroll if an overlay had locked it.
    const d = document.documentElement, b = document.body;
    if (removed) {
      for (const el of [d, b]) {
        if (!el) continue;
        const ov = getComputedStyle(el).overflowY;
        if (/hidden|clip/.test(ov) && d.scrollHeight > innerHeight * 1.3) el.style.setProperty('overflow-y', 'auto', 'important');
      }
    }
  } catch (e) {}
  return removed;
})()`;

// ─── Scroll-jacked experience fingerprint (wheel-driven WebGL timelines) ─────
export const JS_DOC_LOCKED = `(() => {
  try {
    const d = document.documentElement, b = document.body;
    const ov = (getComputedStyle(d).overflowY + ' ' + (b ? getComputedStyle(b).overflowY : '')).toLowerCase();
    return /hidden|clip/.test(ov) && d.scrollHeight <= innerHeight * 1.5;
  } catch (e) { return false; }
})()`;

export const JS_HAS_BIG_FIXED_CANVAS = `(() => {
  try {
    if (document.documentElement.scrollHeight > innerHeight * 1.5) return false;
    for (const c of document.querySelectorAll('canvas')) {
      const r = c.getBoundingClientRect(), s = getComputedStyle(c);
      if (r.width >= innerWidth * 0.6 && r.height >= innerHeight * 0.6 && (s.position === 'fixed' || s.position === 'absolute')) return true;
    }
    return false;
  } catch (e) { return false; }
})()`;

// Scroll position + max (window scroller or the main inner scroll container).
export const JS_SCROLL_STATE = `(() => {
  try {
    const se = document.scrollingElement || document.documentElement;
    return { y: Math.round(se.scrollTop || window.scrollY || 0), max: Math.max(0, Math.round(se.scrollHeight - innerHeight)), h: Math.round(se.scrollHeight) };
  } catch (e) { return { y: 0, max: 0, h: 0 }; }
})()`;

// ─── Reveal mode (before the full-page bands) ────────────────────────────────
// Forces content that only appears on scroll into its visible end state:
//  • every recorded IntersectionObserver target reported as intersecting;
//  • GSAP ScrollTrigger animations to progress(1), triggers disabled without revert;
//  • finite Web Animations finished, infinite ones paused;
//  • known reveal libraries' "in view" classes (AOS, Locomotive, WOW, sal, Webflow);
//  • lazy images made eager.
// Returns counters for the capture log.
export const JS_REVEAL = `(async () => {
  const out = { io: 0, st: 0, tweens: 0, anims: 0, classes: 0, unhidden: 0, lazy: 0 };
  try {
    const S = window.__shelfy;
    if (S && Array.isArray(S.io)) {
      for (const rec of S.io) {
        try {
          const targets = Array.from(rec.targets || []).filter((t) => t && t.isConnected);
          if (!targets.length) continue;
          const now = performance.now();
          const entries = targets.map((t) => {
            const r = t.getBoundingClientRect();
            return { isIntersecting: true, intersectionRatio: 1, target: t, time: now, boundingClientRect: r, intersectionRect: r, rootBounds: null, isVisible: true };
          });
          rec.cb.call(rec.io, entries, rec.io);
          out.io += entries.length;
        } catch (e) {}
      }
    }
  } catch (e) {}
  try {
    const ST = window.ScrollTrigger;
    if (ST && typeof ST.getAll === 'function') {
      for (const t of ST.getAll()) {
        try { if (t.animation) { t.animation.progress(1); t.animation.pause(); } } catch (e) {}
        try { t.disable(false); } catch (e) {}
        out.st++;
      }
    }
    const g = window.gsap;
    if (g && g.globalTimeline && typeof g.globalTimeline.getChildren === 'function') {
      for (const tw of g.globalTimeline.getChildren(false, true, true)) {
        try {
          if (typeof tw.repeat === 'function' && tw.repeat() === -1) continue;
          if (typeof tw.isActive === 'function' && tw.isActive()) { tw.progress(1); tw.pause(); out.tweens++; }
        } catch (e) {}
      }
    }
  } catch (e) {}
  try {
    if (document.getAnimations) {
      for (const a of document.getAnimations()) {
        try {
          const timing = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
          if (timing && timing.endTime === Infinity) { a.pause(); } else { a.finish(); }
          out.anims++;
        } catch (e) {}
      }
    }
  } catch (e) {}
  try {
    const add = (sel, cls) => { for (const el of document.querySelectorAll(sel)) { if (!el.classList.contains(cls)) { el.classList.add(cls); out.classes++; } } };
    add('[data-aos]', 'aos-animate');
    add('[data-scroll]', 'is-inview');
    add('[data-sal]', 'sal-animate');
    add('.wow', 'animated');
    add('[data-animate]', 'animated');
    add('.reveal', 'is-visible');
    add('[data-reveal]', 'is-revealed');
  } catch (e) {}
  try {
    // Elements a reveal script left at opacity:0 via INLINE style. Only elements
    // carrying a reveal marker are touched, never menus/tooltips/slides.
    const sel = '[data-aos],[data-scroll],[data-sal],.wow,[data-animate],[data-reveal],.reveal,[data-w-id],[data-framer-appear-id],[class*="fade" i],[class*="reveal" i],[class*="appear" i],[class*="animate-in" i]';
    let guard = 0;
    for (const el of document.querySelectorAll(sel)) {
      if (guard++ > 4000) break;
      const st = el.style;
      if (!st) continue;
      const inlineOpacity = st.opacity !== '' ? parseFloat(st.opacity) : NaN;
      const cs = getComputedStyle(el);
      if (cs.display === 'none') continue;
      if ((inlineOpacity === 0 || parseFloat(cs.opacity) < 0.02) && !el.closest('nav, [role="menu"], [role="dialog"], [aria-hidden="true"]')) {
        st.setProperty('opacity', '1', 'important');
        if (st.transform) st.setProperty('transform', 'none', 'important');
        if (cs.visibility === 'hidden') st.setProperty('visibility', 'visible', 'important');
        out.unhidden++;
      }
    }
  } catch (e) {}
  try {
    for (const img of document.querySelectorAll('img')) {
      try {
        if (img.loading === 'lazy') { img.loading = 'eager'; out.lazy++; }
        const ds = img.getAttribute('data-src') || img.getAttribute('data-lazy-src') || img.getAttribute('data-original');
        if (ds && (!img.getAttribute('src') || /^data:/.test(img.getAttribute('src') || ''))) { img.src = ds; out.lazy++; }
        const dss = img.getAttribute('data-srcset') || img.getAttribute('data-lazy-srcset');
        if (dss && !img.getAttribute('srcset')) { img.srcset = dss; out.lazy++; }
      } catch (e) {}
    }
    for (const src of document.querySelectorAll('picture source[data-srcset]')) {
      try { if (!src.getAttribute('srcset')) { src.srcset = src.getAttribute('data-srcset'); out.lazy++; } } catch (e) {}
    }
    for (const el of document.querySelectorAll('[data-bg],[data-background-image]')) {
      try {
        const u = el.getAttribute('data-bg') || el.getAttribute('data-background-image');
        if (u && !el.style.backgroundImage) { el.style.backgroundImage = 'url("' + u.replace(/"/g, '') + '")'; out.lazy++; }
      } catch (e) {}
    }
    for (const v of document.querySelectorAll('video')) {
      try {
        if (v.readyState < 2) { v.preload = 'auto'; v.load(); }
      } catch (e) {}
    }
  } catch (e) {}
  try {
    await Promise.race([
      Promise.all(Array.from(document.images).filter((im) => !im.complete && im.decode).map((im) => im.decode().catch(() => {}))),
      new Promise((r) => setTimeout(r, 4000)),
    ]);
  } catch (e) {}
  return out;
})()`;

// Virtualised smooth-scroll wrappers (GSAP ScrollSmoother, Locomotive v3, bespoke
// translate wrappers) pin the document to the viewport; restore native flow so
// the full-page height is real. Lenis uses native scroll and is left alone.
export const JS_NEUTRALIZE_VIRTUAL_SCROLL = `(() => {
  const out = { libs: [] };
  const restore = (el) => {
    if (!el) return;
    try {
      for (const [k, v] of [['transform', 'none'], ['translate', 'none'], ['position', 'static'], ['height', 'auto'], ['min-height', '0'], ['overflow', 'visible']]) el.style.setProperty(k, v, 'important');
    } catch (e) {}
  };
  try {
    if (window.ScrollSmoother && typeof window.ScrollSmoother.get === 'function') {
      const s = window.ScrollSmoother.get();
      if (s) { try { s.scrollTop(0); } catch (e) {} try { s.kill(); } catch (e) {} out.libs.push('scrollsmoother'); }
    }
  } catch (e) {}
  const sc = document.querySelector('#smooth-content, [data-smooth-content], .smooth-content');
  if (sc) { restore(sc); restore(document.querySelector('#smooth-wrapper, [data-smooth-wrapper], .smooth-wrapper')); if (!out.libs.includes('scrollsmoother')) out.libs.push('scrollsmoother-dom'); }
  const lc = document.querySelector('[data-scroll-container]');
  if (lc) { restore(lc); out.libs.push('locomotive'); }
  try {
    const d = document.documentElement, b = document.body;
    const docScrolls = d.scrollHeight > innerHeight + 4;
    if (!docScrolls || /hidden|clip/.test(getComputedStyle(b).overflow + ' ' + getComputedStyle(d).overflow)) {
      for (const el of Array.from(b.children)) {
        try {
          const t = getComputedStyle(el).transform;
          if (el.scrollHeight > innerHeight * 1.2 && t && t !== 'none' && /matrix|translate/.test(t)) { restore(el); if (!out.libs.length) out.libs.push('generic-virtual'); }
        } catch (e) {}
      }
    }
    // Inner scroll container: html/body locked at 100vh and a child scrolling.
    if (d.scrollHeight <= innerHeight + 4) {
      let best = null, bestH = 0;
      for (const el of document.querySelectorAll('body > *, body > * > *, main')) {
        const s = getComputedStyle(el);
        if (!/(auto|scroll)/.test(s.overflowY)) continue;
        if (el.scrollHeight > el.clientHeight * 1.3 && el.clientHeight >= innerHeight * 0.7 && el.scrollHeight > bestH) { best = el; bestH = el.scrollHeight; }
      }
      if (best) {
        let el = best;
        while (el && el !== b) { restore(el); el = el.parentElement; }
        out.libs.push('inner-scroller');
      }
    }
    const lockedY = /hidden|clip/.test(getComputedStyle(d).overflowY + ' ' + getComputedStyle(b).overflowY);
    if (out.libs.length || lockedY) {
      for (const el of [d, b]) {
        el.style.setProperty('overflow-y', 'visible', 'important');
        el.style.setProperty('overflow-x', 'hidden', 'important');
        el.style.setProperty('height', 'auto', 'important');
      }
      if (getComputedStyle(b).position === 'fixed') b.style.setProperty('position', 'static', 'important');
    }
  } catch (e) {}
  try {
    // GSAP pin spacers keep their reserved scroll distance after the end state is
    // forced, which leaves blank gaps in the middle of the page.
    for (const sp of document.querySelectorAll('.pin-spacer')) {
      sp.style.setProperty('padding-top', '0', 'important');
      sp.style.setProperty('padding-bottom', '0', 'important');
      sp.style.setProperty('height', 'auto', 'important');
    }
  } catch (e) {}
  return out;
})()`;

// Full-page pass only: small fixed decorations that would otherwise be painted
// once at the top of the page (custom cursors, cursor followers, scroll hints).
export const JS_HIDE_FLOATING_DECOR = `(() => {
  let n = 0;
  try {
    for (const el of document.querySelectorAll('body *')) {
      const s = getComputedStyle(el);
      if (s.position !== 'fixed') continue;
      const r = el.getBoundingClientRect();
      if (r.width >= 140 || r.height >= 140) continue;
      if (el.querySelector('a, button, nav, img[alt]') && r.top < 120) continue; // header logo/menu button
      if (s.pointerEvents === 'none' || /cursor|follower|pointer|mouse|blob|circle/i.test((el.className && String(el.className)) + ' ' + el.id)) {
        el.style.setProperty('visibility', 'hidden', 'important');
        n++;
      }
    }
  } catch (e) {}
  return n;
})()`;

// ─── Page probe ───────────────────────────────────────────────────────────────
// One pass that returns everything the metadata step needs from a page:
// head metadata, hero anatomy, readable text digest, links for discovery,
// typography from VISIBLE text, colour roles, design tokens, layout traits,
// motion/3D markers, sections, social links, credits and award links.
export const JS_PAGE_PROBE = `(() => {
  const R = {};
  const vw = innerWidth, vh = innerHeight;
  const docTop = () => (window.scrollY || 0);
  const clean = (s, n) => String(s || '').replace(/\\s+/g, ' ').trim().slice(0, n || 300);
  const abs = (u) => { try { return new URL(u, location.href).href; } catch (e) { return ''; } };
  const visible = (el) => {
    try {
      if (!el || !el.getBoundingClientRect) return false;
      if (el.checkVisibility && !el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) return false;
      const r = el.getBoundingClientRect();
      return r.width > 1 && r.height > 1;
    } catch (e) { return false; }
  };
  const region = (el) => {
    const c = el.closest('header, nav, footer, aside, [role="navigation"], [role="banner"], [role="contentinfo"]');
    if (!c) return 'main';
    const t = c.tagName.toLowerCase(), role = c.getAttribute('role');
    if (t === 'footer' || role === 'contentinfo') return 'footer';
    if (t === 'nav' || role === 'navigation') return 'nav';
    if (t === 'header' || role === 'banner') return 'header';
    return 'aside';
  };
  const parseColor = (() => {
    const cv = document.createElement('canvas'); cv.width = cv.height = 1;
    const cx = cv.getContext('2d', { willReadFrequently: true });
    return (c) => {
      if (!c || c === 'transparent' || c === 'none') return null;
      const m = /rgba?\\(([^)]+)\\)/.exec(c);
      if (m) {
        const p = m[1].split(/[ ,\\/]+/).filter(Boolean).map(Number);
        if (p.length >= 3) return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
      }
      try {
        cx.clearRect(0, 0, 1, 1); cx.fillStyle = '#000'; cx.fillStyle = c; cx.fillRect(0, 0, 1, 1);
        const d = cx.getImageData(0, 0, 1, 1).data;
        return { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
      } catch (e) { return null; }
    };
  })();
  const hex = (c) => '#' + [c.r, c.g, c.b].map((v) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, '0')).join('');

  // ── head ──
  try {
    const meta = (sel) => { const el = document.head && document.head.querySelector(sel); return el ? clean(el.getAttribute('content'), 600) : ''; };
    R.head = {
      title: clean(document.head && document.head.querySelector('title') ? document.head.querySelector('title').textContent : document.title, 300),
      lang: clean(document.documentElement.getAttribute('lang'), 20),
      description: meta('meta[name="description" i]'),
      ogTitle: meta('meta[property="og:title" i]'),
      ogDescription: meta('meta[property="og:description" i]'),
      ogImage: abs(meta('meta[property="og:image" i]') || meta('meta[property="og:image:url" i]') || meta('meta[name="twitter:image" i]')),
      ogType: meta('meta[property="og:type" i]'),
      ogSiteName: meta('meta[property="og:site_name" i]'),
      twitterSite: meta('meta[name="twitter:site" i]'),
      twitterCreator: meta('meta[name="twitter:creator" i]'),
      applicationName: meta('meta[name="application-name" i]'),
      themeColor: meta('meta[name="theme-color" i]'),
      colorScheme: meta('meta[name="color-scheme" i]'),
      generator: Array.from(document.querySelectorAll('meta[name="generator" i]')).map((m) => clean(m.getAttribute('content'), 120)).filter(Boolean).slice(0, 5),
      canonical: abs((document.querySelector('link[rel="canonical" i]') || {}).href || ''),
      manifest: abs((document.querySelector('link[rel="manifest" i]') || {}).href || ''),
      icons: Array.from(document.querySelectorAll('link[rel~="icon" i], link[rel="apple-touch-icon" i], link[rel="mask-icon" i]')).slice(0, 12).map((l) => ({ href: abs(l.getAttribute('href')), rel: l.getAttribute('rel'), sizes: l.getAttribute('sizes') || '', type: l.getAttribute('type') || '' })).filter((x) => x.href),
      hreflang: Array.from(document.querySelectorAll('link[rel="alternate" i][hreflang]')).slice(0, 60).map((l) => ({ lang: l.getAttribute('hreflang'), href: abs(l.getAttribute('href')) })),
      rss: abs((document.querySelector('link[type="application/rss+xml" i], link[type="application/atom+xml" i]') || {}).href || ''),
      jsonld: Array.from(document.querySelectorAll('script[type="application/ld+json" i]')).slice(0, 12).map((s) => (s.textContent || '').slice(0, 30000)),
    };
  } catch (e) { R.head = {}; }

  // ── visible text: typography + text colours + digest ──
  const styles = new Map();
  const textColors = new Map();
  let totalChars = 0;
  try {
    const PUA = /[\\uE000-\\uF8FF]/g;
    const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    let n, guard = 0;
    const perEl = new WeakMap();
    while ((n = walker.nextNode()) && guard++ < 40000) {
      const raw = n.nodeValue; if (!raw) continue;
      const t = raw.trim(); if (!t) continue;
      const el = n.parentElement; if (!el) continue;
      if (/^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|SVG|TITLE)$/.test(el.tagName)) continue;
      let info = perEl.get(el);
      if (info === undefined) {
        info = null;
        if (visible(el)) {
          const s = getComputedStyle(el);
          const r = el.getBoundingClientRect();
          info = { s, top: r.top + docTop() };
        }
        perEl.set(el, info);
      }
      if (!info) continue;
      const s = info.s;
      const pua = (t.match(PUA) || []).length;
      if (pua > t.length / 2) continue; // icon font glyphs
      const chars = t.length;
      totalChars += chars;
      const size = parseFloat(s.fontSize) || 16;
      const key = [s.fontFamily, s.fontWeight, s.fontStyle, Math.round(size * 10) / 10, s.lineHeight, s.letterSpacing, s.textTransform].join('|');
      let st = styles.get(key);
      if (!st) {
        st = { family: s.fontFamily, weight: s.fontWeight, style: s.fontStyle, size, lineHeight: s.lineHeight, letterSpacing: s.letterSpacing, transform: s.textTransform, chars: 0, minTop: Infinity, tags: {}, sample: '' };
        styles.set(key, st);
      }
      st.chars += chars;
      st.minTop = Math.min(st.minTop, info.top);
      const tag = el.closest('h1') ? 'h1' : el.closest('h2') ? 'h2' : el.closest('h3') ? 'h3' : el.closest('h4,h5,h6') ? 'h4' : el.closest('button, [role="button"]') ? 'button' : el.closest('nav') ? 'nav' : el.closest('a') ? 'a' : el.closest('code, pre, kbd') ? 'code' : el.closest('p, li, blockquote, td, dd') ? 'p' : 'other';
      st.tags[tag] = (st.tags[tag] || 0) + chars;
      if (!st.sample || (st.sample.length < 40 && t.length > st.sample.length)) st.sample = t.slice(0, 80);
      const c = parseColor(s.color);
      if (c && c.a > 0.3) {
        const h = hex(c);
        textColors.set(h, (textColors.get(h) || 0) + chars * size * size);
      }
    }
  } catch (e) {}
  R.typeStyles = Array.from(styles.values()).sort((a, b) => b.chars - a.chars).slice(0, 40).map((s) => ({ ...s, minTop: Number.isFinite(s.minTop) ? Math.round(s.minTop) : null }));
  R.textColors = Array.from(textColors.entries()).sort((a, b) => b[1] - a[1]).slice(0, 12).map(([hex, w]) => ({ hex, weight: w }));
  R.totalChars = totalChars;

  // ── loaded font faces ──
  try {
    const faces = [];
    if (document.fonts && document.fonts.forEach) {
      document.fonts.forEach((f) => { if (faces.length < 120) faces.push({ family: String(f.family || '').replace(/^["']|["']$/g, ''), weight: f.weight, style: f.style, status: f.status, display: f.display }); });
    }
    R.fontFaces = faces;
  } catch (e) { R.fontFaces = []; }

  // ── backgrounds, gradients, CTAs ──
  try {
    const bgs = new Map(); const gradients = []; const ctas = [];
    const pageArea = Math.max(1, document.documentElement.scrollWidth * document.documentElement.scrollHeight);
    let guard = 0;
    for (const el of document.querySelectorAll('body, body *')) {
      if (guard++ > 12000) break;
      if (!visible(el)) continue;
      const s = getComputedStyle(el);
      const r = el.getBoundingClientRect();
      const area = r.width * r.height;
      const c = parseColor(s.backgroundColor);
      if (c && c.a > 0.6 && area > 2000) {
        const h = hex(c); bgs.set(h, (bgs.get(h) || 0) + area / pageArea);
      }
      if (s.backgroundImage && /gradient/.test(s.backgroundImage) && area > 4000 && gradients.length < 12) gradients.push(s.backgroundImage.slice(0, 300));
      const isBtn = el.matches('button, [role="button"], a, input[type="submit"]') && r.width >= 60 && r.width <= 420 && r.height >= 28 && r.height <= 90;
      if (isBtn && c && c.a > 0.6 && ctas.length < 30) {
        const txt = clean(el.innerText || el.value || el.getAttribute('aria-label'), 60);
        if (txt) ctas.push({ text: txt, bg: hex(c), fg: (() => { const f = parseColor(s.color); return f ? hex(f) : null; })(), radius: s.borderRadius, top: Math.round(r.top + docTop()), href: el.getAttribute('href') ? abs(el.getAttribute('href')) : '' });
      }
    }
    R.backgrounds = Array.from(bgs.entries()).sort((a, b) => b[1] - a[1]).slice(0, 16).map(([hex, coverage]) => ({ hex, coverage: Math.round(coverage * 1000) / 1000 }));
    R.gradients = gradients;
    R.ctas = ctas;
    const rootBg = parseColor(getComputedStyle(document.documentElement).backgroundColor);
    const bodyBg = parseColor(getComputedStyle(document.body).backgroundColor);
    R.canvasBg = (bodyBg && bodyBg.a > 0.5) ? hex(bodyBg) : (rootBg && rootBg.a > 0.5) ? hex(rootBg) : '#ffffff';
  } catch (e) { R.backgrounds = []; R.gradients = []; R.ctas = []; }

  // ── design tokens (CSS custom properties on :root/body) ──
  try {
    const tokens = [];
    const seen = new Set();
    const collect = (el) => {
      const cs = getComputedStyle(el);
      for (let i = 0; i < cs.length && tokens.length < 400; i++) {
        const name = cs[i];
        if (!name.startsWith('--') || seen.has(name)) continue;
        seen.add(name);
        const value = cs.getPropertyValue(name).trim().slice(0, 160);
        if (value) tokens.push({ name, value });
      }
    };
    collect(document.documentElement); if (document.body) collect(document.body);
    R.tokens = tokens;
  } catch (e) { R.tokens = []; }

  // ── layout traits ──
  try {
    const T = { grid: 0, flex: 0, backdrop: 0, blend: 0, textGradient: 0, sticky: 0, fixedHeader: false, radius: {}, shadows: 0, maxWidths: {}, cursorNone: false, marquee: 0, horizontalScroll: 0, svgCount: document.querySelectorAll('svg').length, videos: 0, canvases: 0, iframes: document.querySelectorAll('iframe').length, images: 0, forms: document.querySelectorAll('form').length };
    let guard = 0;
    for (const el of document.querySelectorAll('body *')) {
      if (guard++ > 10000) break;
      const s = getComputedStyle(el);
      if (s.display === 'grid' || s.display === 'inline-grid') T.grid++;
      if (s.backdropFilter && s.backdropFilter !== 'none') T.backdrop++;
      if (s.mixBlendMode && s.mixBlendMode !== 'normal') T.blend++;
      if ((s.webkitBackgroundClip === 'text' || s.backgroundClip === 'text') && /gradient|url/.test(s.backgroundImage)) T.textGradient++;
      if (s.position === 'sticky') T.sticky++;
      if (s.boxShadow && s.boxShadow !== 'none') T.shadows++;
      const r = el.getBoundingClientRect();
      if (s.position === 'fixed' && r.top <= 2 && r.width >= vw * 0.8 && r.height < 160 && el.querySelector('a, nav, button')) T.fixedHeader = true;
      if (s.borderRadius && s.borderRadius !== '0px' && r.width > 60 && r.height > 30) { const k = s.borderRadius.split(' ')[0]; T.radius[k] = (T.radius[k] || 0) + 1; }
      if (s.maxWidth && s.maxWidth !== 'none' && r.width > vw * 0.5) { T.maxWidths[s.maxWidth] = (T.maxWidths[s.maxWidth] || 0) + 1; }
      if (el.tagName === 'VIDEO' && r.width > 200) T.videos++;
      if (el.tagName === 'CANVAS' && r.width > 200) T.canvases++;
      if (el.tagName === 'IMG' && r.width > 120) T.images++;
      if (s.overflowX === 'auto' || s.overflowX === 'scroll') { if (el.scrollWidth > el.clientWidth * 1.5 && r.height > 200) T.horizontalScroll++; }
    }
    try { T.cursorNone = getComputedStyle(document.body).cursor === 'none' || getComputedStyle(document.documentElement).cursor === 'none'; } catch (e) {}
    try {
      if (document.getAnimations) {
        for (const a of document.getAnimations()) {
          try {
            const t = a.effect && a.effect.getComputedTiming && a.effect.getComputedTiming();
            const kf = a.effect && a.effect.getKeyframes ? a.effect.getKeyframes() : [];
            if (t && t.endTime === Infinity && kf.some((k) => /translate/.test(String(k.transform || '')))) T.marquee++;
          } catch (e) {}
        }
      }
    } catch (e) {}
    R.traits = T;
  } catch (e) { R.traits = {}; }

  // ── motion / 3D / framework markers (window + DOM) ──
  try {
    const W = window, D = document, M = {};
    const ver = (v) => (typeof v === 'string' || typeof v === 'number') ? String(v).slice(0, 20) : true;
    if (W.gsap) M.gsap = ver(W.gsap.version);
    if (W.ScrollTrigger) M.scrolltrigger = true;
    if (W.ScrollSmoother) M.scrollsmoother = true;
    if (W.Lenis || W.lenis || D.documentElement.classList.contains('lenis')) M.lenis = ver(W.lenisVersion || (W.Lenis && W.Lenis.version) || true);
    if (W.LocomotiveScroll || D.querySelector('[data-scroll-container]') || D.documentElement.classList.contains('has-scroll-smooth')) M.locomotive = true;
    if (W.barba) M.barba = ver(W.barba.version);
    if (W.swup || D.querySelector('#swup')) M.swup = true;
    if (W.Highway) M.highway = true;
    if (W.THREE || W.__THREE__) M.three = ver(W.__THREE__ || (W.THREE && W.THREE.REVISION));
    if (W.PIXI) M.pixi = ver(W.PIXI.VERSION);
    if (W.BABYLON) M.babylon = true;
    if (W.OGL) M.ogl = true;
    if (D.querySelector('spline-viewer') || W.SPLINE) M.spline = true;
    if (D.querySelector('[data-us-project]') || W.UnicornStudio) M.unicornstudio = true;
    if (W.lottie || W.bodymovin || D.querySelector('lottie-player, dotlottie-player, [data-animation-type="lottie"]')) M.lottie = true;
    if (W.rive || D.querySelector('canvas[data-rive], rive-canvas')) M.rive = true;
    if (W.AOS || D.querySelector('[data-aos]')) M.aos = true;
    if (W.Swiper || D.querySelector('.swiper, .swiper-container')) M.swiper = true;
    if (W.Splide || D.querySelector('.splide')) M.splide = true;
    if (W.SplitType || D.querySelector('[data-split], .split-type, .line-mask')) M.splittext = true;
    if (W.Webflow || D.documentElement.getAttribute('data-wf-site')) M.webflow = ver(D.documentElement.getAttribute('data-wf-site') ? true : true);
    if (W.__NEXT_DATA__ || D.getElementById('__next') || W.next) M.next = ver(W.next && W.next.version);
    if (W.__NUXT__ || W.$nuxt || D.getElementById('__nuxt')) M.nuxt = true;
    if (W.___gatsby || D.getElementById('___gatsby')) M.gatsby = true;
    if (D.querySelector('astro-island, [data-astro-cid]') || Array.from(D.querySelectorAll('[class]')).slice(0, 400).some((e) => /astro-[a-z0-9]{8}/.test(e.className))) M.astro = true;
    if (W.__remixContext) M.remix = true;
    if (W.__sveltekit || D.querySelector('[data-sveltekit-preload-data], [data-sveltekit-hydrate]')) M.sveltekit = true;
    if (W.Vue || W.__VUE__ || D.querySelector('[data-v-app], [data-server-rendered]')) M.vue = true;
    if (W.angular || D.querySelector('[ng-version]')) M.angular = ver((D.querySelector('[ng-version]') || {}).getAttribute ? D.querySelector('[ng-version]').getAttribute('ng-version') : true);
    if (W.React || W.__REACT_DEVTOOLS_GLOBAL_HOOK__ || Array.from(D.querySelectorAll('body *')).slice(0, 300).some((e) => Object.keys(e).some((k) => k.startsWith('__reactFiber') || k.startsWith('__reactContainer')))) M.react = true;
    if (W.jQuery) M.jquery = ver(W.jQuery.fn && W.jQuery.fn.jquery);
    if (W.Shopify) M.shopify = ver(W.Shopify.theme && W.Shopify.theme.name);
    if (D.querySelector('[data-framer-hydrate-v2], [data-framer-name], #__framer-badge-container') || /framerusercontent/.test(D.documentElement.innerHTML.slice(0, 200000))) M.framer = true;
    if (D.querySelector('meta[name="generator" i][content*="Wix" i]') || W.wixBiSession) M.wix = true;
    if (W.Static && W.Static.SQUARESPACE_CONTEXT) M.squarespace = true;
    if (D.querySelector('link[href*="wp-content"], script[src*="wp-content"], script[src*="wp-includes"]')) M.wordpress = true;
    if (W.elementorFrontend || D.querySelector('.elementor')) M.elementor = true;
    if (D.querySelector('[class*="tw-"], .container.mx-auto') || Array.from(getComputedStyle(D.documentElement)).some((p) => p.startsWith('--tw-'))) M.tailwind = true;
    if (W.ga || W.gtag || W.dataLayer) M.gtm = true;
    if (W.plausible) M.plausible = true;
    if (W.posthog) M.posthog = true;
    if (W.Intercom) M.intercom = true;
    R.markers = M;
    const S = W.__shelfy;
    R.canvasContexts = S && S.ctx ? S.ctx : {};
  } catch (e) { R.markers = {}; R.canvasContexts = {}; }

  // ── hero anatomy (first viewport) ──
  try {
    let headline = null;
    for (const st of R.typeStyles || []) {
      if (st.minTop !== null && st.minTop < vh && (!headline || st.size > headline.size)) headline = st;
    }
    const h1 = document.querySelector('h1');
    R.hero = {
      headline: h1 && visible(h1) ? clean(h1.innerText, 200) : headline ? headline.sample : '',
      headlineSize: headline ? headline.size : null,
      ctas: (R.ctas || []).filter((c) => c.top < vh).slice(0, 4),
      media: (() => {
        const kinds = [];
        for (const el of document.querySelectorAll('video, canvas, img, picture, svg, iframe')) {
          const r = el.getBoundingClientRect();
          if (r.top >= vh || r.bottom <= 0 || r.width * r.height < vw * vh * 0.12) continue;
          kinds.push(el.tagName.toLowerCase());
        }
        return Array.from(new Set(kinds));
      })(),
    };
  } catch (e) { R.hero = {}; }

  // ── headings & digest ──
  try {
    R.headings = Array.from(document.querySelectorAll('h1, h2, h3')).filter(visible).slice(0, 40).map((h) => ({ level: Number(h.tagName[1]), text: clean(h.innerText, 160), top: Math.round(h.getBoundingClientRect().top + docTop()) })).filter((h) => h.text);
    const navLabels = [];
    for (const a of document.querySelectorAll('header a, nav a, [role="navigation"] a')) {
      const t = clean(a.innerText || a.getAttribute('aria-label'), 40);
      if (t && !navLabels.includes(t) && visible(a)) navLabels.push(t);
      if (navLabels.length >= 16) break;
    }
    R.navLabels = navLabels;
    const blocks = [];
    let chars = 0;
    const seen = new Set();
    const root = document.querySelector('main') || document.body;
    for (const el of root.querySelectorAll('h1, h2, h3, p, li, blockquote, figcaption, dt, dd')) {
      if (el.closest('nav, footer, [aria-hidden="true"], [role="navigation"], [role="dialog"]')) continue;
      if (!visible(el)) continue;
      let t = clean(el.innerText, 400);
      if (!t || t.length < 3) continue;
      if (/^(?:\\S\\s){6,}\\S?$/.test(t)) t = t.replace(/\\s/g, ''); // split-letter animation text
      const k = t.toLowerCase();
      if (seen.has(k)) continue;
      seen.add(k);
      blocks.push({ tag: el.tagName.toLowerCase(), text: t });
      chars += t.length;
      if (chars > 6000 || blocks.length > 120) break;
    }
    R.blocks = blocks;
  } catch (e) { R.headings = []; R.navLabels = []; R.blocks = []; }

  // ── links (discovery, socials, credits, awards) ──
  try {
    const links = []; const social = []; const credits = []; const awards = [];
    const SOCIAL = [['instagram', /instagram\\.com/], ['x', /(?:twitter|x)\\.com/], ['linkedin', /linkedin\\.com/], ['facebook', /facebook\\.com/], ['youtube', /youtube\\.com/], ['tiktok', /tiktok\\.com/], ['dribbble', /dribbble\\.com/], ['behance', /behance\\.net/], ['github', /github\\.com/], ['vimeo', /vimeo\\.com/], ['pinterest', /pinterest\\./], ['threads', /threads\\.net/], ['bluesky', /bsky\\.app/]];
    const AWARD = /(awwwards\\.com|cssdesignawards\\.com|thefwa\\.com|godly\\.website|land-book\\.com|siteinspire\\.com|onepagelove\\.com|webbyawards\\.com|cssda\\.|csswinner\\.com|httpster\\.net|lapa\\.ninja|bestwebsite\\.gallery|minimal\\.gallery)/i;
    let guard = 0;
    for (const a of document.querySelectorAll('a[href]')) {
      if (guard++ > 3000) break;
      const href = abs(a.getAttribute('href'));
      if (!/^https?:/.test(href)) continue;
      const text = clean(a.innerText || a.getAttribute('aria-label') || a.getAttribute('title'), 80);
      const reg = region(a);
      const vis = visible(a);
      if (links.length < 400) links.push({ href, text, region: reg, visible: vis, top: vis ? Math.round(a.getBoundingClientRect().top + docTop()) : null });
      for (const [p, re] of SOCIAL) { if (re.test(href) && social.length < 20 && !social.some((s) => s.href === href)) social.push({ platform: p, href }); }
      if (AWARD.test(href) && awards.length < 30) {
        const img = a.querySelector('img, svg');
        awards.push({ href, text, region: reg, fixed: getComputedStyle(a).position === 'fixed' || !!a.closest('[style*="position: fixed"], [style*="position:fixed"]'), img: img ? (img.getAttribute('src') || img.getAttribute('alt') || img.tagName.toLowerCase()) : '' });
      }
      if (reg === 'footer' && /\\b(site|website|design|designed|developed|made|crafted|built)\\s+(by|with)\\b|\\b(sito|design|sviluppo|realizzato)\\s+(da|by)\\b/i.test(clean(a.parentElement ? a.parentElement.innerText : '', 160))) {
        if (credits.length < 6 && text) credits.push({ text, href });
      }
    }
    for (const el of document.querySelectorAll('[id*="awwwards" i], [class*="awwwards" i]')) {
      if (awards.length >= 30) break;
      const a = el.closest('a') || el.querySelector('a');
      if (a) awards.push({ href: abs(a.getAttribute('href')), text: clean(a.innerText, 60), region: region(a), fixed: getComputedStyle(el).position === 'fixed', img: 'ribbon' });
    }
    R.links = links; R.social = social; R.credits = credits; R.awardLinks = awards;
  } catch (e) { R.links = []; R.social = []; R.credits = []; R.awardLinks = []; }

  // ── sections ──
  try {
    const H = document.documentElement.scrollHeight;
    let rootEl = document.querySelector('main') || document.body;
    // Descend through single-child wrappers (Framer/Webflow/Next roots).
    for (let i = 0; i < 6; i++) {
      const kids = Array.from(rootEl.children).filter((c) => visible(c) && c.getBoundingClientRect().height > 40);
      if (kids.length === 1 && kids[0].getBoundingClientRect().height > H * 0.6) rootEl = kids[0]; else break;
    }
    const candidates = [];
    const pushBlock = (el, depth) => {
      const r = el.getBoundingClientRect();
      if (r.height < 120 || r.width < vw * 0.6) return;
      const kids = Array.from(el.children).filter((c) => visible(c) && c.getBoundingClientRect().height >= 120 && c.getBoundingClientRect().width >= vw * 0.6);
      if (r.height > Math.max(2400, vh * 2.6) && kids.length >= 2 && depth < 3) { for (const k of kids) pushBlock(k, depth + 1); return; }
      candidates.push(el);
    };
    const header = document.querySelector('body > header, header[role="banner"], body header');
    for (const c of Array.from(rootEl.children)) if (visible(c)) pushBlock(c, 0);
    const footer = Array.from(document.querySelectorAll('footer')).filter(visible).pop();
    if (footer && !candidates.some((c) => c === footer || c.contains(footer) || footer.contains(c))) candidates.push(footer);
    const sections = [];
    const textOf = (el) => (el.innerText || '').toLowerCase();
    for (const el of candidates) {
      const r = el.getBoundingClientRect();
      const top = Math.round(r.top + docTop()), height = Math.round(r.height);
      if (sections.some((s) => Math.abs(s.top - top) < 40 && Math.abs(s.height - height) < 40)) continue;
      const tx = textOf(el).slice(0, 4000);
      const imgs = el.querySelectorAll('img, picture, svg').length;
      const hs = Array.from(el.querySelectorAll('h1, h2, h3')).filter(visible);
      const heading = hs.length ? clean(hs[0].innerText, 100) : '';
      let kind = 'content';
      const isFooter = el.matches('footer, [role="contentinfo"]') || (el.querySelector('footer') && top + height >= H - 40);
      if (isFooter) kind = 'footer';
      else if (el.matches('header, nav') || (top < 10 && height < 160 && el.querySelector('nav, a'))) kind = 'navigation';
      else if (top < vh * 0.95 && height >= 280 && !sections.some((s) => s.kind === 'hero')) kind = 'hero';
      else if (/(pricing|prezzi|piani|plans)\\b/.test(tx) && /[€$£]\\s?\\d|\\d\\s?[€$£]|\\/\\s?(mo|month|mese|year|anno)/.test(tx)) kind = 'pricing';
      else if (el.querySelector('details summary') || /\\b(faq|frequently asked|domande frequenti)\\b/.test(tx.slice(0, 400))) kind = 'faq';
      else if (el.querySelectorAll('blockquote').length >= 1 || (tx.match(/[“”]/g) || []).length >= 4) kind = 'testimonials';
      else if (el.querySelector('form') && el.querySelectorAll('input, textarea').length >= 2) kind = 'form';
      else if (imgs >= 5 && tx.length < 300 && height < 420) kind = 'logos';
      else if ((tx.match(/\\d+(?:[.,]\\d+)?\\s?(?:%|\\+|k\\b|m\\b|x\\b|mln|bn)/gi) || []).length >= 3 && tx.length < 700) kind = 'stats';
      else if (imgs >= 6 && el.querySelectorAll('a').length >= 4) kind = 'gallery';
      else if (el.querySelectorAll('h3, h4').length >= 3) kind = 'features';
      else if (height < vh * 0.9 && hs.length <= 2 && el.querySelector('a, button') && tx.length < 500) kind = 'cta';
      else if (el.querySelector('video, canvas') && tx.length < 300) kind = 'media';
      sections.push({ kind, top, height, heading });
      if (sections.length >= 24) break;
    }
    R.sections = sections.sort((a, b) => a.top - b.top);
    R.docHeight = H;
  } catch (e) { R.sections = []; R.docHeight = document.documentElement.scrollHeight; }

  return R;
})()`;
