# Website Analyzer (v2)

Paste a URL → Shelfy captures the site like a design-reference library would (Godly, Refero,
Awwwards), measures its design system, and catalogs it with a vision model. Audit of the v1
pipeline and the reasons for each change: [website-analyzer-audit.md](website-analyzer-audit.md).

## Pipeline

| Step | Module | What happens |
|---|---|---|
| Queue | `electron/weborchestrator.ts` | Placeholder card, one site at a time, job events/progress, `blocked` state |
| Capture | `electron/webcap/capture.ts` | Per page: navigate → anti-bot check → consent opt-out + pop-up removal → loader/fonts/stability → **untouched hero @2×** → (primary) **scroll video** → reveal mode + page probe → **full page in 2000-px bands @2×** (filmstrip for scroll-jacked WebGL) → footer + **section crops** |
| Page choice | `electron/webcap/discover.ts` | Links of the rendered header/nav/footer first, sitemap only as supplement; page-type classifier (EN/IT/FR/DE/ES); legal/auth/cart/careers/search and language variants excluded; one page per type (two case studies) |
| Drivers | `electron/webcap/driver.ts`, `cdp-page.ts`, `electron-driver.ts`, `system-chrome.ts` | Sandboxed headless Chromium (Playwright, one context per site, 1440×900 @2×); Electron window fallback; the user's real Chrome for anti-bot checks |
| Blocking | `electron/webcap/blocker.ts`, `build/prepare-adblock.ts` | SSRF guard first, then EasyList/EasyPrivacy/EasyList Cookie/Peter Lowe + Shelfy rules (chat, pop-ups, accessibility overlays), compiled at build time; cosmetic CSS per frame; autoconsent (opt-out) |
| In-page probes | `electron/webcap/scripts.ts` | Challenge detection, loader detection, reveal (IntersectionObserver registry, GSAP end states, WAAPI finish, reveal-library classes), overlay removal, page probe (head, visible typography, colours, CTAs, tokens, layout traits, motion/3D markers, sections, links, socials, credits, award links) |
| Metadata | `electron/webcap/metadata.ts` | Palette from pixels (OKLab k-means) joined with DOM roles, scheme and WCAG contrast; fonts actually rendering visible text (roles, weights, sizes, provider from font-file hosts, classification, type scale); ~80 tech fingerprints with versions; awards validated against the site's own domain; JSON-LD; title cleaning; traits |
| AI catalog | `electron/webcap/ai-catalog.ts` (+ `analyzer.ts` `analyzeWebCatalog`) | Captioned images (hero hi-res, whole-page overview, distinctive sections, footer, inner heroes), GROUND TRUTH block, untrusted page text; schema v2 with closed vocabularies; mapped to the legacy `ai_*` columns + `ai_web_json` + `post_facets` |
| Browse/search | `electron/db.ts` (`getWebFacetCounts`, `queryWebReferences`, `similarWebReferences`) | Facet counts, text over every field (fonts, tech, catalog), colour search by perceptual distance, similar sites by weighted facet overlap + palette proximity |

## Assets per site

`<userData>/assets/web/<stamp>-<host>-<hash>.*` — per page: `hero` (2880×1800 WebP), full-page
bands, `footer`, section crops; primary page: scroll video (H.264 MP4) + hover preview.
Everything is listed in `posts.web_pages_json` (per page) and `web_meta_json.video`.

## Anti-bot checks

A page showing a Cloudflare/DataDome/PerimeterX/Akamai/… interstitial is never saved. The job
goes to `blocked`; "Supera la verifica" opens the user's real Chrome (dedicated profile,
`<userData>/webcapture-browser`). Nothing is attached while the user passes the check; then the
capture runs **in that same tab** over a low-footprint DevTools connection (no `Runtime.enable`,
no `Network.enable`, no injected scripts, no device emulation — each of those revokes the
clearance on some sites). Inner pages are attempted in the same tab and skipped if challenged
again. Shelfy never solves a check itself.

## Stored data

| Column / table | Content |
|---|---|
| `web_pages_json` | per page: url, pageType, title, status, hero, chunks (bands), footer, sections, heightCss, capped, jacked, qc, digest, contentText |
| `web_palette_json` | swatches `{hex, name, role, coverage, source, oklch}` |
| `web_fonts_json` | `{family, role, roles, weights, sizes, share, provider, classification, sample}` |
| `web_tech_json` | technology names (details with category/version in `web_meta_json.tech`) |
| `web_awards_json` | `{platform, level, profileUrl, evidence, confidence}` |
| `web_meta_json` | `schema: 2`, identity, scheme, contrast, type scale, tech, traits, video, socials, credits, capture provenance |
| `ai_web_json` | the v2 catalog (`Shelfy.WebAiCatalog`) |
| `post_facets` | `(post_id, facet, value)` for filters and similar sites |

## Tests

`tests/electron/webcap.test.ts` (page choice, colour/typography/tech/award/JSON-LD metadata,
catalog mapping) and `tests/electron/webcap-scripts.test.ts` (in-page scripts in a real
headless Chromium on synthetic pages).
