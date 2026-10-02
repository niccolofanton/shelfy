# Website Analyzer — audit (2026-10-02)

Scope: the whole "paste a URL → capture → metadata → AI catalog → browse/search" feature.
Method: full code read of `electron/weborchestrator.ts`, `webcapture*.ts`, `capture-engine.ts`,
`web-enrich.ts`, the web path of `analyzer.ts`, `db.ts` web columns, `src/views/AiWebsites.tsx`;
in-page probes re-run in the bundled Chromium on synthetic pages; a real end-to-end run on 6 sites
with the remote vision node (Ornith · Qwen 27B Vision).

Severity: **C** critical · **H** high · **M** medium · **L** low.

## 1. Real-run results (6 sites, current code)

| Site | Outcome | What went wrong |
|---|---|---|
| linear.app | done | AI saw the top 2000 px of home, about, pricing, **contact** at 287×448 px. Generic copy ("sleek dark mode aesthetic"). Palette: 3 near-identical greys + accents at weight 0. Fonts: Inter only as "heading", provider "unknown". |
| stripe.com | done | Good font name (sohne-var) but provider "unknown". Description boilerplate ("clean, high-end aesthetic"). |
| lusion.co | done | Capture good (WebGL filmstrip). AI says "deep black backgrounds" — the page is light lavender with a dark card (hallucination at 448 px). **Award attributed from a client case page** (`awwwards.com/sites/oryzo-ai`). Inner pages = a single project case. |
| aesop.com | **saved a Cloudflare challenge page** | QC passed it; AI described "a security verification interstitial" and catalogued it as e-commerce/beauty. |
| itsnicethat.com | done | Palette: `#2b2b2b` text 0.99, everything else weight 0. Tech empty. |
| apple.com/airpods-pro | done | 14 "fonts" of which 11 are icon fonts. Tech empty. Pasted deep URL ignored (discovery restarts from origin). |

Throughput: 6 sites ≈ 17.5 min (≈ 3 min/site, sites strictly sequential).

## 2. Weaknesses

### A. Capture fidelity

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| A1 | C (security) | Playwright Chromium runs **without sandbox** although the comment says "sandbox stays ON": `chromiumSandbox` is never set, so Playwright adds `--no-sandbox`. Arbitrary sites/ads render unsandboxed in a never-updated headless shell. | webcapture-playwright.ts:348-368; playwright-core coreBundle.js:41681 | `chromiumSandbox: true` (env opt-out kept), `serviceWorkers: 'block'`. |
| A2 | H | No untouched above-the-fold shot: every asset is taken after cookie clicks, animation kill, autoscroll, forced GSAP end states and fixed→static. The "hero" is the first ≤2000 px band of that mutated page. | webcapture-playwright.ts:859-1062, :1084 | Shoot a 1440×900 @2x viewport right after readiness + consent, before any DOM mutation; store as `role=hero`; use it for thumbnail and AI. |
| A3 | H | `JS_NEUTRALIZE_FIXED` flattens fixed/sticky to static: overlay headers push the hero down, hidden drawers/modals become in-flow gaps, undismissed cookie bars become bands, small fixed design elements are hidden. Unneeded with captureBeyondViewport. | webcapture.ts:1475-1502 | Don't flatten on the CDP full-page path; remove only dialogs/backdrops and elements invisible at scroll 0; flatten only in the OSR fallback. |
| A4 | H | `animation:none!important` removes `fill-mode: forwards` end states → headlines revealed by keyframes render invisible; blocked by strict CSP on some sites (inconsistent). | webcapture.ts:1263-1272 | `document.getAnimations()` → finish finite, pause infinite; drop the CSS kill. |
| A5 | H | Scroll-reveal content is captured un-revealed except for GSAP (AOS, Framer appear, Locomotive, Motion `whileInView`, Webflow IX2). | webcapture.ts:1442-1471 | Init-script `IntersectionObserver` shim reporting all targets intersecting + library end-state hooks. |
| A6 | H | 1× DPR at 1280 px: blurry on Retina, 1× srcset assets, narrower breakpoint than the 1440 standard. | webcapture-playwright.ts:727; webcapture.ts:76-77 | 1440×900 @2x, band capture (`clip`) to stay under surface limits. |
| A7 | H | No mobile capture; the AI prompt claims one exists. | analyzer.ts:1612, :1866 | Mobile context 393×852 @3x, `isMobile`, `hasTouch`, mobile UA, for home + 1-2 key pages. |
| A8 | H | Pages > 12 000 px are cut silently; no footer shot; `capped` not persisted. | webcapture.ts:78; webcapture-playwright.ts:1016-1051 | Band capture with ~30 k cap + dedicated footer viewport shot; persist real height and `capped`. |
| A9 | H | Consent/popups/chat: one-shot click script before the preloader wait, can't reach iframes/shadow DOM, can click a navigating `<a>`, always "accept all"; adblock has ~100 hosts and no cosmetic filtering, no popup/chat vendors. | webcapture.ts:1211-1261; adblock.ts:18-121 | Maintained consent handling (opt-out) + cosmetic filter lists (EasyList/Annoyances/Cookie), generic modal killer + Escape, re-run right before each shot. |
| A10 | M | Lazy media partially handled (220 ms steps, only `img[data-src]`, unloaded videos stay black). | webcapture.ts:1392-1426 | Adaptive waits, `picture source[data-srcset]`, `data-bg`, `data-lazy-src`, video `load()`/poster. |
| A11 | M | Pinned GSAP sections leave blank mid-page gaps (pin-spacer padding kept). | webcapture.ts:1453 | Collapse `.pin-spacer` padding after forcing end states. |
| A12 | M | Sites that scroll an inner container are captured as one viewport. | webcapture-playwright.ts:1003-1015 | Detect and unwrap the main scroll container. |
| A13 | M | Canvas probe calls `getContext('webgl2')` on every canvas → claims canvases the site later wants as 2D, can exhaust WebGL contexts. | webcapture.ts:1375-1388 | Record context requests via init-script `getContext` wrapper; never create contexts. |
| A14 | L | Fixed backgrounds (`background-attachment: fixed`, fixed WebGL) appear only behind the first viewport. | captureBeyondViewport | Stitch scrolled tiles when a full-viewport fixed background is detected. |
| A15 | L | Encoding: 1 px rescale on odd heights blurs the whole image; lossy 4:2:0 WebP bleeds coloured text; failed bands skipped silently. | webcapture.ts:937, :1011, :1028-1031 | No rescale when not shrinking; high-quality/near-lossless masters; report failures. |

### B. Page discovery

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| B1 | H | Sitemap always wins; nav links used only if the sitemap yields < 2 URLs. Yoast/Shopify sitemaps list posts/products first → home + random posts. | webcapture.ts:716-736, :621-628 | Primary source = rendered header/nav/footer links; sitemap as supplement (round-robin, `<priority>`). |
| B2 | H | No denylist: contact, privacy, terms, login, cart, careers, tag/category/feed pages are eligible (linear: contact + contact/sales; monogrid: 2 job ads). | webcapture.ts:251-283 | Page-type classifier + denylist; rank by visual value (home → work/case → product/pricing → about). |
| B3 | M | Language variants captured as pages when the default language is at root; `?lang=` dupes kept. | webcapture.ts:386-389 | Exclude hreflang alternates. |
| B4 | M | `extractLinks` matches `<link href>` → `/feed`, `/wp-json`, `/xmlrpc.php` candidates. | webcapture.ts:633-648 | Only `<a href>` from the rendered DOM. |
| B5 | M | Pasted deep URL ignored in multi-page mode (apple.com/airpods-pro → apple.com home). | webcapture.ts:671-680 | Always include the pasted URL as page #1. |
| B6 | M | Discovery uses Node `fetch` (bot TLS fingerprint → 403 on Cloudflare/Akamai), sequential sitemaps in a 15 s budget → silent home-only. | webcapture.ts:459, :719-723; weborchestrator.ts:232 | Harvest from the rendered page; fetch sitemaps in-page; parallel; 30 s budget. |
| B7 | M | No post-capture checks: off-origin redirects, auth redirects, non-HTML, near-duplicate pages. | — | Drop them; perceptual-hash dedup. |

### C. Robustness, anti-bot, QC, job state

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| C1 | C | Challenge/blocked pages saved as references (aesop.com real run). HTTP status never read; no challenge/login-wall detection; UA hard-coded Chrome/124 on Chromium 148 (bot fingerprint). | webcapture-playwright.ts:822-827; webcapture.ts:61-63 | Reject status ≥ 400/non-HTML; detect Cloudflare/Akamai/DataDome/captcha/login; mark the job **blocked** with a clear reason; UA from `browser.version()`; optional visible-window retry where the user passes the check. |
| C2 | H | Pause/cancel corrupts job state: `runPool` rejects on first abort while other workers keep running and re-set status `capturing` → paused job dropped, cancelled job "capturing" forever, site can't be re-added. | weborchestrator.ts:361-380, :650-659, :1078-1092 | Run id per job, ignore stale patches, `allSettled` before rethrow. |
| C3 | M | QC: 448 px judgement, biased categories ("mostly dark" = black), mid band sent as "top portion"; if hero still flagged, og:image replaces **every band**; `usedOgImage` not persisted; no QC without a model. | analyzer.ts:1533-1539; weborchestrator.ts:688-704, :823-840 | Pixel/DOM-first QC (variance, edges, visible text, loader present), VLM only as tie-break, `blocked` category, og image as separate `role=og` asset, persist verdicts. |
| C4 | M | `goto(waitUntil:'load', 45s)` fails slow pages; every page pays up to 20 s `networkidle`. | webcapture-playwright.ts:822, :832 | `domcontentloaded` + race(load ≤ 15 s, visual stability + fonts + LCP). |
| C5 | M | Loader check wastes 30 s on off-screen curtain preloaders. | webcapture-playwright.ts:501-514 | Require viewport intersection + `elementFromPoint` hit. |
| C6 | M | Filmstrip path ignores `settleBeforeShotMs`; QC re-capture "with longer wait" is a no-op exactly for WebGL sites. | webcapture-playwright.ts:903-957, :982 | Apply settle/stability before frame 0. |
| C7 | M | Home failure ⇒ no palette/fonts/tech and an inner page becomes the hero; failed pages never retried. | weborchestrator.ts:583-604, :1035 | Retry once relaxed; metadata from first good page; merge across pages. |
| C8 | M | OSR fallback: silent, renders in `persist:social`, session-wide header hook races across concurrent captures, redirects not SSRF-checked, WebGL blank. | capture-engine.ts:139-164; webcapture.ts:1698, :1801, :1841 | Throwaway partition, per-webContents header capture, SSRF on redirects, record engine used. |
| C9 | M (privacy) | All `persist:social` cookies (Instagram/X logins) injected into every capture → third-party embeds act logged-in; account-flagging risk. | webcapture-playwright.ts:445-473, :736-742 | Import nothing by default. |
| C10 | L (security) | SSRF guard checks literal hostnames only; service workers/WebSockets bypass `route`; untrusted og:image bytes go to ffmpeg with format auto-detect; 2 MB truncation produces corrupt images. | net-safety.ts:26; webcapture.ts:505-510, :811-818 | Block SW, resolver-level checks, magic-byte sniffing + forced demuxer, reject truncated bodies. |

### D. Metadata extraction

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| D1 | C | Palette measures DOM boxes, not pixels: only `html,body,header,nav,main,section,[class*=hero]` backgrounds; ignores gradients, images, video, canvas, SVG, footer, divs. Real runs: greys only, accents at weight 0. | web-enrich.ts:985-992, :973 | Pixel OKLab area-weighted k-means over all bands + visible-DOM role join; store `{hex, oklch, role, coverage, source}` + `scheme`. |
| D2 | H | Body text colour dominates (weighted by box area: itsnicethat `#2b2b2b` 0.99). | web-enrich.ts:988 | Weight text by glyph coverage; separate text-colour list. |
| D3 | H | No perceptual merge: `#111111` and `#121212` take two slots. | web-enrich.ts:1042, :1070 | ΔE-OK merge, diversity-aware top-N, reserved accent slot. |
| D4 | M | Light/dark threshold wrong (`#aaaaaa` = "background-dark"); alpha and hidden elements counted (hidden `#00ff00` link in palette). | web-enrich.ts:968-975, :1066 | OKLab L threshold; composite rgba; `checkVisibility()`. |
| D5 | M | Screenshot fallback palette inverted (5 % accent outranks 85 % background). | web-enrich.ts:1014, :1113-1121 | Replaced by D1. |
| D6 | H | Font provider is page-global: one gstatic preconnect makes every family "google"; external @font-face invisible; real runs: all "unknown". | web-enrich.ts:1314-1325 | Map family → font files via `response` events (resourceType font) + CSSOM; provider by file host (Google, Adobe, Fontshare, Monotype, H&Co, Webflow, Framer, self-hosted). |
| D7 | H | Fonts are declared names, not rendered faces: next/font hashes, `*_Fallback`, `Inter Placeholder`, icon fonts (apple: 11 icon fonts of 14), `Apple Color Emoji` as heading. | web-enrich.ts:1260-1283, :1372-1375 | Only `loaded` faces used by visible text; strip hashes/fallbacks/placeholders/icon fonts; optional CDP `getPlatformFontsForNode`. |
| D8 | H | Font roles from the first DOM node (`querySelector('h1')`): sr-only h1 decides the heading font; one role per family. | web-enrich.ts:1272-1283, :1357-1366 | Visible-text walker aggregating (family, weight, size, line-height, tracking, case) → display/body/mono + type scale. |
| D9 | H | Tech detection: 19 regex rules; bundled chunks never match filename regexes; React root, `__THREE__`, Lenis missed in headless; new Webflow host missed; no categories/versions. Real runs: apple/itsnicethat empty. | web-enrich.ts:1381-1466, :1421 | Declarative fingerprints over DOM + request URLs + window probes with versions (~80 curated entries; Wappalyzer data is GPL — do not bundle). |
| D10 | M | Generator meta → junk tech ("powered"), only first tag read; text regexes give prose false positives ("we build with three.js"). | web-enrich.ts:1441, :1478-1547 | Alias map; match only script/request URLs. |
| D11 | H | Capture signals computed then thrown away: smooth-scroll lib, ScrollTrigger counts, preloader, WebGL/canvas, scroll-jacking, page height, trackers blocked, discovery `templateHint`. | webcapture-playwright.ts:833-1010; webcapture.ts:251-320 | Persist a `signals` object (motion, webgl, height, pageType). |
| D12 | M | Metadata home-only, all-or-nothing. | weborchestrator.ts:583-604 | Run probes on every page, merge (home ×2). |
| D13 | M | Awards: any footer image containing "awwwards" = badge; outbound links to another site's entry = self-award (lusion real run); `\bfwa\b` matches any token; one award per platform; first `<footer>` only. | web-enrich.ts:1580-1606, :1669-1788 | Badge only inside anchor to the platform entry path or from the badge CDN; slug≈domain for self-links; one award per (platform, level). |
| D14 | M | Award tags/entities computed and never used; `awardsQualityBoost` unused. | weborchestrator.ts:965-968; web-enrich.ts:1940 | Write gated award tags/entities deterministically. |
| D15 | H | Main text picks the wrong block (`<main>` → first `<article>` → body); hero inside `<header>` deleted; `aria-hidden` menus, marquees ×8, "Skip to content" kept; split-letter text kept. | web-enrich.ts:535-571 | In-page `innerText` of visible blocks, readability scoring, dedupe, keep hero text and structure. |
| D16 | H | Inner-page text never reaches the model: site aggregate capped at 1200 chars home-first; title+description prepended again (monogrid: meta description ×5, ~825 of 1200 chars). | web-enrich.ts:219-220; db.ts:2353 | Budgeted per-page digest (type, H1, H2s, key sentence, CTA labels), no duplication. |
| D17 | M | JSON-LD allowlist exact-match (Corporation/Restaurant/IRI types dropped). | web-enrich.ts:239-254, :514-517 | Normalize IRIs, subtype ancestry, lift `sameAs`/`logo`/`address`. |
| D18 | M | Thin/raw meta: relative og:image unresolved and not stored locally; raw title ("Home | Acme") used as name; SVG `<title>` can become page title; no favicon/theme-color/canonical/hreflang. | web-enrich.ts:452-455, :733, :779 | Resolve + download og/icon locally; title cleaning; siteName cascade. |
| D19 | M (security) | Prompt sanitizer misses Unicode Tag chars (U+E0000–E007F) and bidi isolates; `stripTags` on decoded text deletes prose ("<50 EUR"). | web-enrich.ts:409-415 | Strip `\p{Cf}` + tag block; decode once. |
| D20 | M | `web_domain` inconsistent (www vs no-www, og:site_name fallback). | weborchestrator.ts:935; db.ts:2340, :2486 | Always `webHostname(finalUrl)`. |
| D21 | M | No unit tests for web-enrich (module mocked as `{}`). | weborchestrator-recover.test.ts:56 | Fixture corpus (~20 saved sites) + in-page probe tests. |

### E. AI cataloging

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| E1 | C | **The model sees thumbnails**: ≤ 4 images = `chunk[0]` of the first 4 discovered pages, scaled to 448 px long side → 287×448; 16 px text ≈ 3.5 px. Nothing below 2000 px is seen. The remote 27B gets the same. | analyzer.ts:320-321, :1366-1395, :1612-1626 | Provider-aware image budget: hero viewport ≥ 1024 px (remote 1536), whole-page contact sheet, 2 inner heroes, mobile; captions per image. |
| E2 | C | **Bulk re-analysis sends web references down the social/video path** ("Analizza" in gallery, "Analizza mancanti"): light posts lack `platform`/`web_*`, runJob keys on `platform==='web'`. | db.ts:2045-2068; ipc.ts:568-584, :1029-1041; analyzer.ts:3780 | Hydrate web fields / key on `media_type='website'`; test. |
| E3 | C | Extracted metadata not used as grounding: palette, fonts, awards, JSON-LD, page types never reach the prompt, yet the prompt asks the model to describe palette and typography. | analyzer.ts:1857-1912 | Trusted GROUND TRUTH block; "never contradict or invent beyond it". |
| E4 | C | Schema has no design facets (style, theme, layout, type, hero type, imagery, components) — everything crammed into a 1-2 sentence description + 10 free tags. | analyzer.ts:1914-1946 | Schema v2 with closed vocabularies (evidence-first field order). |
| E5 | H | `purpose` enum has no `other` though prompt and comment say it does → strict grammar forces a wrong label. | analyzer.ts:1818-1830 vs :1900 | Add `other`. |
| E6 | H | Taxonomy mixes genre/business model/page type (landing vs saas, agency vs portfolio); industry lacks design-creative, art-culture, music, film, photography, hospitality, luxury, AI/dev tools; overlapping values across enums. | analyzer.ts:1818-1855 | Versioned taxonomy v2 (+ secondary label, confidence). |
| E7 | H | Prompt produces generic prose: no specificity rules, no banned adjectives, `save_reason` invites praise, description first in the schema. Real runs: "clean, high-end aesthetic", "sleek dark mode". | analyzer.ts:1706, :1902-1907 | Design-curator role; element+position+treatment rule; banned words; description generated last. |
| E8 | H | Image selection/labels: `ROLE_ORDER` sorts on a role never persisted; prompt claims footer/mobile; images uncaptioned. | analyzer.ts:1612-1626, :1866; db.ts:2383-2385 | Persist role/pageType; caption every image. |
| E9 | H | No controlled vocabulary: example tags baked in and copied; motion tags ("scroll-telling") asked from stills; synonyms fragment (dark mode / dark theme). | analyzer.ts:1904; :1950-1969 | Enum facets; canonical-tag normalization; motion only from deterministic evidence. |
| E10 | M | No hallucination guards; capture artifacts (black WebGL areas, og fallback) hidden from the model. | weborchestrator.ts:698 | Pass capture flags; "black/blank areas are capture artifacts"; `unknown` allowed. |
| E11 | M | Output limits: 768 max_tokens, no `maxItems`/`maxLength`, silent truncation, uncapped entities/keywords. | analyzer.ts:2046-2047, :2203-2213 | Schema constraints; ~1.5 k tokens. |
| E12 | M | Entities/awards not deterministic (tech reaches entities only if the model copies it). | analyzer.ts:1886; db.ts:2392 | Merge tech, fonts, awards, organization in code. |
| E13 | M | Language: always English output; `language` guessed though `<html lang>` known; enum slugs shown raw in the Italian UI. | analyzer.ts:1706, :1908; AiWebsites.tsx:1151-1155 | Language from metadata; i18n labels for enums. |
| E14 | M | Remote path not tuned (thinking flag hard-coded to one model id; same 448 px budget). | analyzer.ts:331 | Per-provider settings. |
| E15 | H | No evaluation harness for web cataloging. | scripts/ | Frozen gold set + metrics + A/B judge. |

### F. Storage, search, UI

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| F1 | H | Purpose/industry/style not filterable (Gallery hard-codes category/contentType `undefined`; per-category stats unused). | Gallery.tsx:226-227; db.ts:1448-1456, :3053-3062 | Facet filters for web references. |
| F2 | H | Free-text search ignores entities, fonts, tech, palette, purpose/industry, save_reason; inner-page text not searchable. | db.ts:1546, :1559-1562 | Index all of it (FTS) + colour search by ΔE. |
| F3 | M | Websites view is a pipeline monitor: text list, 112 px tiles labelled by path, 3-line clamped description, inert chips, purpose/sector hidden until restart, streamed tags never shown (parser looks for `"tags"`), favicons via Google S2 (leaks the list, fails offline). | AiWebsites.tsx:236-239, :596-668, :763-847, :1345-1384 | Visual grid + detail view; clickable pivots; local favicons; fix streaming keys. |
| F4 | M | No "similar sites" (no semantic/visual similarity). | embeddings.ts | e5 over summary+facets; palette/layout similarity. |
| F5 | M | Re-analysis = full re-capture; no AI-only re-catalog; no schema/prompt version stored; purpose/industry not user-editable. | App.tsx:387-401; AiPanel.tsx:443-447 | AI-only re-catalog (single/bulk), `ai_schema_version`, editable facets preserved across re-runs. |
| F6 | L | Raw unversioned slugs in confusingly named columns (`ai_category`/`ai_content_type`). | db.ts | Versioned `ai_web_json` + `post_facets` table; keep old columns as mirrors. |

### G. Performance & operations

| ID | Sev | Weakness | Evidence | Fix |
|---|---|---|---|---|
| G1 | M | Whole capture (Playwright, multi-MB base64, sync fs, regex over multi-MB HTML) runs in the Electron main process → UI stutter. | weborchestrator.ts | Move to a `utilityProcess`. |
| G2 | M | Fresh context per page + `route('**/*')` disables HTTP cache → every page re-downloads bundles/fonts/textures. | webcapture-playwright.ts:725-763 | One context per site; CDP `setBlockedURLs`. |
| G3 | L | Re-captures serial (up to 180 s each); timeout budgets exceed the 150 s page budget (killed with nothing saved, reported as "Target closed"). | weborchestrator.ts:238, :739-850 | Pool re-captures; deadline-aware budget, hero first. |
| G4 | L | Orphaned files on re-capture/og fallback/cancel; no sweep of `assets/web`. | weborchestrator.ts:695-697, :776-780 | Per-job artifact tracking + orphan sweep. |
| G5 | L | Hard-coded Italian event strings in the main process. | weborchestrator.ts:345-348 … | Emit codes, localize in renderer. |
| G6 | M | Too little provenance stored (requested vs final URL, role, viewport/DPR/engine, HTTP status, QC verdict, og flag, skipped pages and why). | weborchestrator.ts:948-955 | Per-asset manifest. |

## 3. Missing capabilities vs pro tools (Godly, Land-book, Awwwards, Refero, SiteInspire)

- Untouched hero @2x as primary asset; mobile hero + full page; footer shot.
- Scroll video (10-20 s smooth-scroll MP4 + animated hover preview).
- Section segmentation (nav, hero, features, social proof, pricing, CTA, footer) as individual crops.
- Design tokens: CSS custom properties, type scale per role, spacing, radii, shadows, colour roles with WCAG contrast.
- Motion/3D facts from evidence (GSAP, Lenis, Locomotive, Barba, Lottie, Rive, Spline, three.js/OGL/R3F).
- Credits ("site by" agency), social links, favicon/OG stored locally.
- Faceted browsing, colour search, "similar sites", clickable pivots.
- Evaluation corpus and regression tests on real sites.
