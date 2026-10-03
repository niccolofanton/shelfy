# Shelfy UX/UI audit and improvement plan (X4a, E8)

Owner decision E8 (2026-10-03): the mobile UI is optimized, and UX/UI design experts review the whole app and bring it to a professional standard **without redesigning it**. This document is the review. Lanes UX-0 to UX-10 (§5) carry out the fixes.

| | |
|---|---|
| Date | 2026-10-03 |
| Base | `web/foundations` at `54184f2` |
| Panel | product design (hierarchy, type, spacing, consistency), UX research (flows, states, copy), mobile interaction (touch, reach, gestures, safe areas), accessibility (contrast, focus, keyboard, screen readers, motion) |
| Web app | `shelfy-server` release build serving `web/dist` on port 18296. Owner account from `admin create-owner`, and a library from `admin synth --posts 2000`: Instagram 1,302, X 697, web 1, one folder ("Saved", 750 posts). Signed in through `admin login-link` and the consent gate. |
| Desktop app | Electron through Playwright with the desktop e2e's mock IPC (`e2e/electron-fixture.ts`, 15 posts). Remote image hosts were blocked, so covers fall back as they do offline or after a CDN URL expires. |
| Viewports | 1440×900, 1024×768, 390×844 (iPhone, touch), 412×915 (Android, touch). The app is dark-only (no `dark:` variant, no `prefers-color-scheme` handling, no theme switch), so every screenshot is dark. |
| Code sweeps | touch targets, accessibility, design-token usage, and states and copy, over `src/` and `web/src/`, with `file:line` evidence |
| Screenshots | 107 PNGs outside the repo in `/Users/fant/work/experiments/shelfy-web-local/ux-audit/shots/`, named `<viewport>-<nn>-<screen>.png`, `electron-*` and `p2-*`. Appendix A lists them. None are committed. |

**Caveats on the synthetic data.** Synthetic posts have no remote URLs. About a third have no stored cover, so they render the "Media unavailable" card. That ratio is realistic: in the reference library 67 % of posts had a local cover. The stored originals are sparse placeholders that no decoder can read, so the post modal's full-size image fails on synthetic posts that do have a cover (MOD-2). Real originals decode, but any failed image (an expired CDN URL, a missing object, a network error) produces the same broken state.

Severity: **P0** is broken or looks broken. **P1** is clearly unprofessional. **P2** is polish. Platforms: **DW** is desktop web (≥900 px), **M** is mobile web (<900 px, the `narrow:` screen), **DA** is the desktop app, and **All** is every platform. Line numbers are from `54184f2`; if one drifts, search by symbol.

---

## 1. Summary: the changes that most raise perceived quality

| # | Change | Why | Sev | Lane |
|---|---|---|---|---|
| 1 | **Replace the floating menu button with one in the layout flow.** | On every phone screen it covers content: the first card and its selection checkbox, the Trash retention line, the Jobs queue chips, the Settings subtitle. It also floats **above the open post modal**. | P0 | UX-1 |
| 2 | **Make the gallery toolbar fit 390 px.** | The toolbar row is 748 px wide inside a 390 px column. Filters, Refresh and Select are off-screen and can't be tapped. When anything focuses them, the whole gallery slides 358 px sideways and stays there. | P0 | UX-3 |
| 3 | **Fix the viewport height and add safe areas.** | The shell uses `100vh`, so iPhone Safari's toolbar can cover the BottomNav. The installed iOS PWA draws under the status bar (`black-translucent`), and only the BottomNav reads a safe-area inset. | P0 | UX-1, UX-2, UX-5 |
| 4 | **Portal the modals and adopt a z-index scale.** | Every view, and so every modal, renders inside App's `zIndex: 2` overlay, so a modal's `z-50` can't beat the shell. The app uses 13 ad hoc z levels. | P0/P1 | UX-2, UX-5 |
| 5 | **Show keyboard focus, and fix the failing grey and accent text tokens.** | Focus is invisible everywhere (`:focus-visible { outline: none }`). The muted token `#606060` is 2.8–3.1:1, `gray-500` is 3.4–4.0:1, and white on the accent is 4.36:1. | P1 | UX-2 |
| 6 | **Fix the media failure states.** | On the "Media unavailable" card, the hover overlay prints the handle a second time over the placeholder text. In the modal, a failed image shows the browser's broken-image glyph with the whole caption as alt text. | P1 | UX-4, UX-5 |
| 7 | **Use context-aware empty states with one shared component.** | One message serves the empty library, no search results, no filter results and an empty folder. On the web it points to desktop-only features ("Import a JSON file… the Browser tab"), in text at 1.9–2.6:1. | P1 | UX-2, UX-3, UX-6 |
| 8 | **Make the selected state visible, and make selection work on mobile.** | The selection ring is an inset shadow painted under the cover, so only a 20 px checkbox shows selection. On phones the Select button is off-screen and the bar's × is clipped. | P1 | UX-3, UX-4 |
| 9 | **Show filters as a sheet on mobile and as an overlay below 1280 px; stop duplicating the sidebar.** | On a phone the panel pushes the grid into a 110 px strip. At 1024 px the cards shrink to about 95 px. The panel repeats the sidebar tree under another name ("Bookmarks"), and shows website-only facets on any library. | P1 | UX-3 |
| 10 | **Run a copy and terminology pass.** | The same concept appears as folder, source and collection ("Create new source"). The Italian UI shows English strings ("Search posts...") and the English desktop UI shows Italian ones ("Veloce", "Il più leggero…"). "Jobs" exposes operator wording, and a raw backend error reaches the UI. | P1 | every lane on its own namespaces |

---

## 2. Findings per screen

### 2.1 Shell: sidebar, drawer and BottomNav (`SH`)

| ID | Finding | Sev | Fix (files, classes, tokens, copy) | Where |
|---|---|---|---|---|
| SH-1 | **The floating menu button covers content on every narrow screen and sits above the post modal.** It covers the first card's selection checkbox, the Trash retention line, the "Bulk action" chip in Jobs and the Settings subtitle (`ios-02`, `ios-10`, `ios-11`, `ios-12-*`), and it shows over the modal (`ios-07-modal`). Cause: `Sidebar.tsx:404-416` is `fixed top-16 left-3 z-40` in the root stacking context, while every view, the PostModal's `z-50` included, renders inside `App.tsx:984`'s `position:absolute; zIndex: 2` overlay. | P0 | Delete the floating trigger. Add `src/components/MenuButton.tsx`: 44×44, `hidden narrow:flex`, `aria-label={t('openMenu')}`, `aria-expanded`, `aria-controls="app-drawer"`. Lift `drawerOpen` from Sidebar to App, shared through a small `ShellContext` (`openMenu()`, `menuOpen`). Each screen renders MenuButton in its top row: the gallery pill row (UX-3), and the Trash, Jobs and Settings headers (UX-6, UX-7). | M |
| SH-2 | **The shell height is `100vh`** (`src/index.css:88-93` `#root`, `App.tsx:904` `h-screen`). On iPhone Safari, 100vh is the *large* viewport, so in a page that never scrolls the BottomNav can sit under Safari's toolbar. Chromium can't emulate this. | P0 (verify) | `#root { height: 100vh; height: 100dvh; }`, and `h-[100dvh]` on App's root. Verify in the iOS Simulator's Safari. | M |
| SH-3 | **The installed iOS PWA draws under the status bar.** `web/index.html` sets `apple-mobile-web-app-status-bar-style=black-translucent` and `viewport-fit=cover`, but the only safe-area inset in the app is `BottomNav.tsx:61`. The toolbar (`Gallery.tsx:1432` `absolute top-0`), the view headers and the drawer header sit under the clock and the notch. Landscape side insets are ignored too. | P0 (PWA) | On App's root: `narrow:pt-[env(safe-area-inset-top)] narrow:pl-[env(safe-area-inset-left)] narrow:pr-[env(safe-area-inset-right)]`. Every fixed full-screen layer pads itself: the drawer (`pt-[env(safe-area-inset-top)]`), and, in their own lanes, the PostModal, the sheets and the lightbox. | M |
| SH-4 | **The drawer is not a dialog.** No `role="dialog"`, no Escape handler anywhere in `Sidebar.tsx`, focus doesn't move in or come back, and the page behind stays reachable. | P1 | `useDialog()` from UX-2 handles focus in, the trap, restoring focus to MenuButton, Escape and `inert` on `<main>`. On the aside while open: `id="app-drawer" role="dialog" aria-modal="true" aria-label={t('menu')}`. Close button 44×44 (now 32: `Sidebar.tsx:441-450`). Width `narrow:w-[min(85vw,320px)]`, `overscroll-contain`. | M |
| SH-5 | **Folders can't be reached by keyboard.** Folder rows are `<div onClick>` (`Sidebar.tsx:318-333`): the probe's 14 Tab stops skip "Saved". The tree chevrons (`source-all-toggle`, `source-instagram-toggle`) have no accessible name. | P1 | Make folder rows `<button type="button">` with `aria-current={active ? 'page' : undefined}`. Chevrons get `aria-label` (expand or collapse) and `aria-expanded`. | All |
| SH-6 | **"Edit folder" (the pencil) is hover-only** (`Sidebar.tsx:365-376`, `hidden group-hover:flex`). Touch can't reach it. | P1 | On narrow, show it always, or open edit on long-press of the row. Hit area 44 px on narrow. | M |
| SH-7 | Folder rows are `w-full mx-2` and overflow the sidebar scroller by 8 px (measured `scrollWidth` 247 vs `clientWidth` 239), which can show a horizontal scrollbar on Windows. | P2 | `w-[calc(100%_-_1rem)]` (underscores, not spaces), or put the margin on the list. | DW, DA |
| SH-8 | "New folder" is styled like a disabled row (dim text and icon, `desk-02`). | P2 | `text-[--text-secondary]` with a hover state, so it reads as an action. | All |
| SH-9 | "X / Twitter" uses the Twitter bird in the sidebar and in the modal header (blue), but the X logo on cards (`desk-02`, `desk-07c`). | P2 | Use the X glyph everywhere in `SourceIcon.tsx`, and label it "X". | All |
| SH-10 | The active "Settings" row is a filled pill without the left accent bar that every other active row has (`desk-12-*`). | P2 | Reuse the same `accentBar`. | DW, DA |
| SH-11 | **The BottomNav "Search" tab does nothing of its own.** It opens the library (`App.tsx:363-375`) and doesn't focus or reveal search. | P1 | Go to the library, then focus the search field, through a `shelfy:focus-search` window event that FilterBar listens to (UX-3). On narrow the search pill becomes full width while it has focus. | M |
| SH-12 | BottomNav labels are 11 px, and inactive tabs use `text-gray-500` (3.91:1 on `#111`). | P2 | `text-xs` (12 px); inactive color `--text-muted` (5.5:1 with the new value). | M |
| SH-13 | `document.title` is always "Shelfy". | P2 | Per view: "Trash · Shelfy", "Saved · Shelfy", "<author> · Shelfy" for a post. Helps tabs, history and screen readers. | DW, M |
| SH-14 | On a 404, the sidebar still highlights "All posts" and the gallery toolbar still shows "2,000 posts" (`desk-14`). | P2 | On `notFound`, no active row and no gallery toolbar. | DW |

### 2.2 Gallery: toolbar, grid, cards, filters, search, sort, count, selection (`GAL`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| GAL-1 | **The toolbar doesn't fit on phones.** The pill row is 748 px wide inside the `overflow-hidden` gallery column (measured at 390: `scrollWidth` 748, `clientWidth` 390). Filters (x 533), Refresh (x 622) and Select (x 656) are off-screen: filters and selection can't be tapped, and long-press is the only way to select. When anything focuses an off-screen control (keyboard, `scrollIntoView`, a screen reader), the column scrolls to `scrollLeft` 358 and stays there, so the grid disappears (`ios-05a-search-tab`). | P0 | Narrow layout of the pill row, in the same pill style: **[MenuButton 44] [Search, flex-1, h-11] [Filters icon, 44, with count badge] [Select icon, 44]**. On narrow, the view mode (Grid/Canvas), sort, density and Refresh move into the filter sheet's new "View" section, and the count becomes the sheet's button ("Show 1,234 posts"). Make the gallery column `overflow-x-clip`: `overflow: clip` can't be scrolled by script. Files: `FilterBar.tsx` (`w-[300px] max-w-[30vw]` becomes `narrow:flex-1 narrow:max-w-none`), `Gallery.tsx` (the leading and trailing slots), `GridSizeControl.tsx` (`narrow:hidden`). | M |
| GAL-2 | **One empty state covers four situations, with the wrong copy on the web.** "No posts found. Import a JSON file or capture posts via the Browser tab." (`gallery.ts:103-104`, `Gallery.tsx:2059-2070`) appears for an empty library, a search with no results, filters with no results and an empty folder. The web has no JSON import and no Browser tab. The text is `#555` and `#444` on `#0f0f0f`: 2.6:1 and 1.9:1. | P1 | Use `EmptyState` (§3.6) with variants. **No results for a search:** `No posts match "{q}"` with [Clear search]. **No results for filters:** "No posts match these filters" with [Reset filters]. **Empty folder:** "This folder is empty" / "Select posts in the library, then choose Add to folder." **Empty library:** on the web, "Save posts from Instagram, X and Pinterest with the browser extension or the Share sheet" with [Set up the extension] (opens Settings → Account → tokens); on the desktop, keep the current hint. | All |
| GAL-3 | **On the "Media unavailable" card, the hover overlay collides with the placeholder.** The handle prints twice and overlaps "Media unavailable" (`desk-03`, `desk-06`, `desk-09`). About a third of cards look like this, so this matters as much as the image cards. The platform icon also appears twice (centered and as a badge), and "Media unavailable" is `text-gray-600` at 10 px (2.3:1). | P1 | In `PostCard.tsx`: when the fallback renders (`:329-339`), the overlay (`:865-941`) drops its handle line and keeps the caption and date. Move the fallback block into the upper half (`justify-start pt-[24%]`). Remove the centered icon (keep the badge). "Media unavailable" becomes `text-[11px] text-[--text-muted]`. | All |
| GAL-4 | **The selected state is invisible except for the checkbox.** `ring-2 ring-inset` (`PostCard.tsx:707`) is an inset box-shadow, painted *under* the cover image, so it never shows (zoomed crop of `desk-06`). | P1 | When selected, add an overlay child: `<span aria-hidden className="pointer-events-none absolute inset-0 z-20 ring-2 ring-inset ring-[--accent] bg-[#7b5cff]/12" />`. Plain RGBA, **no `backdrop-filter`**. Keep `.u-clip-aa` on the card. | All |
| GAL-5 | **Filter panel at 1024–1279 px.** The panel pushes the grid while the column count follows the window width (the comment at `index.css:633-638`), so at 1024 px the cards shrink to about 95 px and text cards are cut mid-line (`tab-04-filters`). | P1 | Below 1280 px, open the panel as an overlay sheet over the grid, with a backdrop. At 1280 px and up, keep the push panel. | DW |
| GAL-6 | **The filter panel repeats the sidebar and shows facets that don't apply.** Its "Bookmarks" section copies the sidebar's tree under a second name. Industry, Site type and AI status show for any library (`desk-04`). Industry, Site type and AI status are native `<select>`s, unlike the segmented controls above them. | P1 | Hide the source mirror while the sidebar is visible (≥900 px); on narrow keep it, titled "Library". Show Industry and Site type only when the source is Websites or the media type is Website, and AI status only with the `ai` capability. Restyle the selects like `Segmented`, or a Popover menu. | All |
| GAL-7 | The media type control wraps to two rows, with "Image" next to "Images" (`desk-04`). | P2 | Rename "Images" to "Image set", and keep the cells on a 4-column grid so they align. | All |
| GAL-8 | Text cards cut mid-line when the cards narrow (`desk-04`, `tab-04`). | P2 | Add a vertical fade mask on the text block, like `.ai-fade-right`, or derive the line clamp from the column density. | All |
| GAL-9 | **Toasts.** Gallery mounts three independent fixed toasts (`Gallery.tsx:1376-1419`). The feedback and job toasts share `bottom-6` and can stack on each other (`handleDeletePosts` sets both). On narrow they land on the BottomNav. `whitespace-nowrap` overflows small screens. The feedback text is the accent on `#1a1a1a` (3.99:1). None has `role="status"` or a dismiss button. | P1 | Use the shared `ToastHost` (§3.6). | All |
| GAL-10 | **The search field.** It is 14 px, so iOS zooms in on focus. It has `type="text"`, with no `enterKeyHint`, `autoCapitalize` or `spellCheck`. The Italian placeholder is English ("Search posts...", `filterBar.ts:15-16`). The clear button is 20×20. | P1 | `type="search" enterKeyHint="search" autoCapitalize="none" spellCheck={false}`, `narrow:text-base`. Clear button: 44 px hit area on narrow. Translate the IT strings. | M, All |
| GAL-11 | **Selection bar on phones.** "3 selected · Select all 2,000 · Actions · ×": the × is clipped at the right edge (`ios-06`), and the controls are 28 px tall. | P1 | On narrow, show the selection bar at the **bottom**, in place of the BottomNav while selecting (as iOS Photos does): [× 44] [N selected] [Select all] [Actions ▾ 44]. On desktop it stays where it is. The bulk Actions menu opens as a bottom sheet (Popover `presentation="auto"`, UX-2). | M |
| GAL-12 | Range select (Shift-click), drag-select, long-press and ⌘± density have no hint anywhere. | P2 | Show a one-time tip in the selection bar: "Shift-click to select a range" on desktop, "Tip: long-press a post to select it" on touch. Add a "Keyboard shortcuts" list (desktop). | All |
| GAL-13 | The empty and loading states don't share a vertical position: the skeleton starts below the toolbar, the empty state is centered in `min-h-[60vh]`. | P2 | Center `EmptyState` in the space below the toolbar (`paddingTop: headerH`, as the skeleton does). | All |

### 2.3 Post modal: media, notes, tags, folders, AI panel (`MOD`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| MOD-1 | **The modal can lose to the shell's stacking.** It renders inside App's `zIndex: 2` overlay (`App.tsx:984`), so any fixed shell element above 2 paints over it (SH-1). | P0 | Render `PostModal` and `ImageLightbox` through `createPortal(…, document.body)` (Popover already does), with z tokens from §3.1. | All |
| MOD-2 | **A failed image shows a broken-image glyph and the whole caption.** `MediaCarousel.tsx:95` and `:224` have no `onError`. The alt text (the full caption) spills over the media pane (`electron-03-modal`, `p2-ios-modal-more-menu`, `p2-desk-modal-folders-popover`). Expired CDN URLs will trigger it in the desktop app. | P1 | `onError` shows the same fallback panel the web shows for a post without media, with the **post's platform icon** (not the globe), "Media not available", [Open original] and [Retry]. `alt` becomes a short label (author and media type), not the caption. | All |
| MOD-3 | **Text-only posts show an empty media pane.** For tweets without media, half the modal is a globe and "Open original" (`desk-07c`, `ios-07c`), and the globe suggests a website. | P1 | For `mediaType === 'text'`, render the text card (quote mark, 20 px text) in the media pane, or collapse to one centered column (`max-w-[640px]`). | All |
| MOD-4 | **The user's own tags and note are labelled as AI output.** They sit inside the "AI categorization" card (`AiPanel.tsx:70-81` Section, plus UserLayer). On the web without AI, the card is titled "AI categorization" and holds nothing from AI (`desk-07`). | P1 | Split it. A "Your tags & note" card comes first, keeping the emerald icons. The AI card renders only with AI content or the `ai` capability; without content it shows "Not analyzed yet" and [Analyze]. | All |
| MOD-5 | **Two sets of arrows on phones.** Post prev/next (44 px, at `top-1/2` of the full-screen modal, which is the media/meta boundary) overlap the carousel dots, and the carousel has its own 36 px arrows inside the media (`ios-07`). | P1 | On narrow: horizontal swipe on the media moves between posts, plus ‹ › chevrons in the header. The carousel moves by swipe (`snap-x snap-mandatory`) and dots. Remove the floating post arrows on narrow. Desktop unchanged. | M |
| MOD-6 | The header icons (folders, more, close) are 32 px (`PostModal.tsx:466-473`, `ActionsMenu.tsx:274-284`, `CollectionsMenu.tsx:34-43`). There is no safe-area top. They are named by `title` only. | P1 | `narrow:w-11 narrow:h-11`. Header `narrow:pt-[env(safe-area-inset-top)]`. Add `aria-label` from the existing strings. | M, All |
| MOD-7 | **Videos autoplay with sound** (`MediaCarousel.tsx:117`, `:218`: `autoPlay` without `muted`) and ignore reduced motion. | P1 | Add `muted playsInline`. Autoplay only when `!useReducedMotion()`. Keep the controls. | All |
| MOD-8 | Focus doesn't return to the card on close, the page behind isn't inert, and the folders and more menus don't take focus. | P1 | `useDialog()`. Menus get `role="menu"`, `role="menuitem"` and arrow-key navigation. | All |
| MOD-9 | The folders popover says "Create new source" (EN) and "Crea nuova source" (IT). | P1 | "New folder" / "Nuova cartella" (§3.7). | All |
| MOD-10 | The AI panel shows the raw backend error: "Error: {job.error}" (`AiPanel.tsx:961`). | P2 | Map the error code to `errors.code.*`; never print `job.error`. | All |
| MOD-11 | The tag input (12 px) and the note (13 px) make iOS zoom, and the keyboard can cover them. | P2 | `narrow:text-base`; `scrollIntoView({ block: 'center' })` on focus on narrow. | M |
| MOD-12 | At 1024 px the post prev/next arrows sit on the modal's edges (`tab-07`). | P2 | Below 1200 px, put them in the header as chevrons, or narrow the modal to leave 64 px gutters. | DW |
| MOD-13 | The more menu offers "Download original" for a post whose media is missing. | P2 | Disable it, with a reason ("No stored file"). | All |

### 2.4 Folders and dialogs (`DLG`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| DLG-1 | **Five dialogs have no dialog semantics**: CollectionModal, AddBookmarkModal, AddSiteModal, ImportModal and ImportFolderModal. No `role="dialog"` or label; ImportModal has no Escape and no focus move; AddBookmarkModal doesn't move focus. None restores focus. | P1 | `useDialog()` plus `role="dialog" aria-modal="true" aria-labelledby`. | All |
| DLG-2 | **Labels aren't tied to their fields.** `htmlFor`/`id` appears once in `src/` (`FeedbackModal.tsx:391,403`). The CollectionModal name, AddBookmarkModal description and AddSiteModal address are unassociated. AddBookmarkModal's tag input loses its only name when the placeholder blanks (`:374`). | P1 | Use `useId()` and `htmlFor`, or wrap the field in the label. | All |
| DLG-3 | Close and remove buttons are icon-only with `title` only, and `ImportModal.tsx:71-76` and `AddBookmarkModal.tsx:361-366` have no name at all. | P1 | `IconButton` (UX-2), which requires `label`. | All |
| DLG-4 | Form errors are plain red `<p>` with no `role="alert"` (AddBookmarkModal, AddSiteModal, ImportModal, ImportFolderModal). | P2 | `Notice tone="error"` with `role="alert"`. | All |
| DLG-5 | Two modal tiers, `z-50` and `z-[60]`, with no rule between them. | P2 | z tokens (§3.1). | All |

### 2.5 Trash (`TR`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| TR-1 | **The empty state is generic and repeats the header.** It uses the gallery's ImageOff icon, and its copy repeats the retention line printed just above it (`desk-10`). | P1 | `EmptyState` with the Trash icon, "Trash is empty", and one line of body. Hide the retention line while the trash is empty. | All |
| TR-2 | "Empty trash" stays visible at 0 items, as a red button at 40 % opacity (`Trash.tsx:313-327`). | P2 | Hide it at `total === 0`. | All |
| TR-3 | The menu button covers the retention line on phones (SH-1). The toasts (`Trash.tsx:272-291`) sit on the BottomNav. | P1 | MenuButton in the header row; `ToastHost`. | M |
| TR-4 | The header pattern (56 px bar, 16 px title, count) differs from Jobs and Settings. | P2 | Shared `PageHeader` (§3.6) for Trash and Jobs: MenuButton (narrow), title `font-display text-lg font-semibold`, count in muted text, actions on the right. | All |
| TR-5 | On narrow, Restore and Delete forever are 32 px, Select all is 28 px, and the trash selection toolbar has the same overflow risk as GAL-11. | P2 | 44 px on narrow; bottom selection bar as in GAL-11. | M |

### 2.6 Jobs (`JOB`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| JOB-1 | **Operator controls on a user screen.** Four queue chips (Bulk action, Library migration, Trash purge, Storage usage), each with unlabeled 24 px pause, cancel and clear icons in low contrast, show even when nothing is queued (`desk-11`, `QueueBar.tsx:55-104`). | P1 | Show a queue only while it has queued or running jobs. Move its controls into a "⋯" menu with text labels ("Pause queue", "Cancel queued", "Clear finished"). | All |
| JOB-2 | The empty state is bare text, "No recent jobs." (`Jobs.tsx:196-199`). | P1 | `EmptyState` with the ListChecks icon: "No background jobs" / "Bulk changes, imports and media downloads show their progress here." | All |
| JOB-3 | Jargon: "Library migration", "Storage usage", "0/3 tries" on a finished job, "Garbage collection" (`jobs.ts:60`, `:110`). Italian "Lavori" reads oddly for jobs. | P2 | Labels such as "Updating storage usage" and "Cleaning up unused files"; show attempts only after a retry ("Attempt 2 of 3"). IT: "Attività". | All |
| JOB-4 | The state chips wrap to two rows on phones, at 28 px. | P2 | One horizontally scrolling row with an edge fade; `narrow:h-9` with 44 px tap padding. | M |

### 2.7 Settings: account, passkeys, sessions, tokens, language, storage, legal (`SET`)

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| SET-1 | **The header crushes the subtitle on phones.** The version pill squeezes it into a column about 100 px wide, and in Italian the pill runs off the right edge (`ios-12-*`, `p2-ios-it-settings`). The pill is developer information in the page's most prominent spot. | P1 | Move the version to an "About" card in the Legal tab. On narrow, the header is the title and a full-width subtitle. | All |
| SET-2 | **Two "Sign out" buttons behave differently.** The Profile one signs out at once (`Account.tsx:90-117`); the Sessions row for this browser asks to confirm (`:333-391`). | P1 | Keep one: Profile's, with a confirm. The current session's row shows "This browser" and no button. | All |
| SET-3 | **"No limit" looks like "full".** With no quota, the storage bar fills to 100 % (`Storage.tsx:38-40` spans the usage itself). | P1 | Without a quota, show a thin stacked breakdown (media and database) labelled "Breakdown", with no track. | All |
| SET-4 | **Re-auth offers a passkey to an account that has none.** "Use a passkey" is the primary button whenever the *server* supports passkeys (`capabilities.passkeys`, `crates/server/src/routes/me/mod.rs:104-106`), so with no passkey the first action fails (`p2-desk-reauth-dialog`). The operator command box is always expanded. With email-only methods, focus goes nowhere (`ReauthDialog.tsx:68-69`). | P1 | When ReauthHost opens, load `GET /me/passkeys`. Show the passkey button only with at least one passkey; otherwise "Email me a link" is primary. Put the command under a disclosure, "Other ways to confirm". Focus the first available button. | All |
| SET-5 | **Italian in the English desktop Settings.** The model cards show "Veloce", "Bilanciato" and "Il più leggero e veloce…" from `electron/analyzer.ts:352-363` (`electron-04-settings`). | P1 | Map model ids to keys in `settings.ts` on the renderer side; leave `electron/` alone (D25). | DA |
| SET-6 | Buttons (`settings/ui.tsx:87-94`, `px-3 py-1.5 text-xs`, about 28 px) and inputs (`h-8`, 14 px) are under 44 px and make iOS zoom. | P2 | `narrow:min-h-11 narrow:px-4 narrow:text-sm`; inputs `narrow:h-11 narrow:text-base`. | M |
| SET-7 | Jargon in user copy: "API tokens" / "Token API", "No tokens.", "…require an active Tailscale peer" (`settings.ts:77`). | P2 | "Access tokens", "No tokens yet. Create one for the browser extension or the iOS Shortcut." Keep Tailscale wording under Advanced (desktop). | All |
| SET-8 | The tabs are 36 px tall; Italian labels are longer. | P2 | `narrow:min-h-11`; horizontal scroll with an edge fade when they overflow. | M |
| SET-9 | The archive rows' icons use three hues (violet, green, blue) while other cards use neutral icons. | P2 | Neutral icons (`text-[--text-secondary]`). | All |

### 2.8 Sign-in, magic link, re-auth link (`AUTH`)

The strongest screens in the app: centered card, a clear primary action, honest copy, and a link that is spent only on click.

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| AUTH-1 | The card is centered vertically, so it jumps about 40 px when the "invalid link" error appears (`desk-01` vs `desk-01b`). | P2 | `AuthLayout.tsx`: anchor at `pt-[16vh]` instead of centering. | All |
| AUTH-2 | The email input is 14 px (iOS zoom), with no `enterKeyHint`, `autoCapitalize` or `spellCheck`. Buttons are `h-10`. | P2 | `narrow:text-base enterKeyHint="send" autoCapitalize="none" spellCheck={false}`; `narrow:h-11`. | M |
| AUTH-3 | "Email me a sign-in link" looks disabled (a dim secondary button), though it's the documented first-time path. The "OR" divider and helper text are 3.4–3.9:1. | P2 | Secondary button: border `--border-strong`, text `--text-primary`. Muted text from the new tokens. | All |

### 2.9 `/share` and `/device`

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| SHR-1 | An unreachable host gets "It no longer exists." (`p2-ios-share-url`, with `example.invalid`), which suggests a deleted page. | P2 | Map the error to "Shelfy couldn't open this address. Check it and try again." | All |
| SHR-2 | The no-link state is clear (`p2-ios-share-nolink`). The buttons are `h-10`. | P2 | `narrow:h-11`. | M |
| DEV-1 | `/device` is good: a 48 px monospace code field, the right input attributes and a clear warning. Only the disabled "Approve" looks muddy. | P2 | Disabled primary: `opacity-50` on the normal fill, rather than a darker fill. | All |

### 2.10 Errors, loading, toasts and the desktop app

| ID | Finding | Sev | Fix | Where |
|---|---|---|---|---|
| ST-1 | Lazy views fall back to a bare centered spinner (`App.tsx:82-88` ViewLoading): content jumps when the view mounts. | P2 | Show a header-shaped skeleton (PageHeader plus three rows), and only after 150 ms. | All |
| ST-2 | While the session is checked, the splash is a static logo with no progress (`web/src/Root.tsx:174-178`); a slow server looks frozen. | P2 | After 800 ms, add a spinner and "Connecting…". After 8 s, show "Can't reach Shelfy" with [Retry]. | DW, M |
| ST-3 | There is no offline signal: a failed fetch becomes "The server is not answering" only where an action reports errors. | P2 | A small "Offline" pill rendered by the shell (`App.tsx`) while `navigator.onLine` is false or the event stream is down. | DW, M |
| ST-4 | The 404 page is the best state pattern in the app: icon, title, body, primary action (`desk-14`). | — | It is the model for `EmptyState`. | — |
| DA-1 | The Downloads toolbar uses Title Case ("Download All", "Download Missing", "Clear finished"). | P2 | Sentence case. | DA |
| DA-2 | Activity log dismiss (✕) is hover-only (`ActivityCenter.tsx:323-333`). | P2 | Always visible at reduced opacity. | DA |

---

## 3. Cross-cutting systems

### 3.1 Tokens: what exists, what is used, and a proposed set

**What exists.** `src/index.css:13-73` defines 10 color tokens, a motion system (`--ease-*`, `--dur-1…4`), and a type scale (`--fs-2xs … --fs-xl`, with `.u-fs-*` utilities). `tailwind.config.ts` adds only the `narrow` screen.

**What is used** (from the code sweep):

| System | Usage |
|---|---|
| Color | About 2,500 hardcoded color expressions (244 distinct) against 198 `var()` references: 12.6 to 1. `--bg-card` is never used through `var()`. Greys: 50+ distinct values for text, 29 for backgrounds and 19 for borders. `text-white` 312×, `text-gray-500` 143×, `text-gray-400` 106×, `#7b5cff` hardcoded 148×. |
| Type | `.u-fs-*` is used **0 times**. 389 arbitrary `text-[Npx]` in 17 sizes (9 to 44 px: 11 px 130×, 10 px 74×, 12 px 53×, 12.5 px 42×, 13 px 36×, 11.5 px 19×), plus 446 Tailwind sizes. 6 letter-spacing variants for one uppercase-eyebrow pattern. |
| Radii | 11 values, about 560 uses. Modals are consistent (`rounded-xl` shell, `rounded-md` close). PostCard alone mixes 4 radii. |
| Shadows | Disciplined: 6 variants. |
| Icons | lucide-react in 63 files. 19 `size` values (13 109×, 14 89×, 12 74×, 15 73×, 16 54×, …) and 8 effective stroke widths. |
| z-index | 13 levels, no scale. `z-50` serves toasts, dialogs, a dropdown and a bottom bar. `z-[60]` is a second modal tier. `z-[120]` is shared by the lightbox, a color popover and ReauthDialog. |
| Primitives | None for Button, IconButton, Input, Modal, EmptyState or Spinner. `settings/ui.tsx` has className constants used by 3 files; Popover (positioning only) by 5. 24 files build their own spinner. |

**Measured contrast** (WCAG 2.x; AA is 4.5:1 for normal text, 3:1 for large text and UI parts):

| Pair | Ratio | Verdict |
|---|---|---|
| `--text-primary #f0f0f0` on `#0f0f0f` | 16.82 | pass |
| `--text-secondary #a0a0a0` on `#0f0f0f` / `#202020` | 7.33 / 6.23 | pass |
| **`--text-muted #606060`** on `#0f0f0f` / `#1a1a1a` | **3.05 / 2.77** | fail |
| **`gray-500 #6b7280`** on `#0f0f0f` / `#111` / `#1a1a1a` / `#202020` | **3.96 / 3.91 / 3.60 / 3.37** | fail |
| **`gray-600 #4b5563`** on `#0f0f0f` / `#1a1a1a` | **2.54 / 2.30** | fail |
| Empty state `#555` / `#444` on `#0f0f0f` | **2.6 / 1.9** | fail |
| **White on `--accent #7b5cff`** (primary buttons, 14 px) | **4.36** | fail, narrowly |
| **`--accent` as text** on `#0f0f0f` / `#1a1a1a` (links, toasts) | **4.40 / 3.99** | fail |
| `violet-400 #a78bfa` on `#1a1a1a` | 6.40 | pass |
| `--error #ef5350` / `--success #4caf50` on `#0f0f0f` | 5.50 / 6.90 | pass |
| `--border #2e2e2e` against `#0f0f0f` | 1.41 | decorative only; fields rely on their fill |

The sweep also found 113 uses of 14 hardcoded greys under 4.5:1 (`#6f6f6f`, `#7a7a7a`, `#6b6b6b`, `#555`, `#444`, …).

**Proposed token set.** It is small, keeps every current surface value, and fixes contrast. Define it in `src/index.css`, and expose it to Tailwind in `tailwind.config.ts` (`theme.extend.colors`, `fontSize`, `zIndex`) so classes like `text-muted` and `z-modal` exist.

| Token | Value | Notes |
|---|---|---|
| `--bg-primary` | `#0f0f0f` | unchanged |
| `--bg-sidebar` | `#111111` | new name for the existing sidebar and BottomNav value |
| `--bg-secondary` / `--bg-card` / `--bg-hover` | `#1a1a1a` / `#202020` / `#2a2a2a` | unchanged |
| `--bg-elevated` | `#1c1c1e` | pills, popovers and sheets (now hardcoded) |
| `--text-primary` / `--text-secondary` | `#f0f0f0` / `#a0a0a0` | unchanged |
| **`--text-muted`** | **`#8a8a8a`** (was `#606060`) | 5.55 on `#0f0f0f`, 5.04 on `#1a1a1a`, 4.72 on `#202020` |
| `--text-disabled` | `#5c5c5c` | disabled controls only (exempt from contrast) |
| `--accent` | `#7b5cff` | unchanged: fills, indicators, the brand |
| **`--accent-fill`** | **`#6d4dff`** | filled buttons with a white label: 5.05:1 (owner decision O2) |
| **`--accent-text`** | **`#a48fff`** | accent text, links and icons on dark: 7.26 / 6.59 / 6.17 / 5.43 on the four surfaces |
| **`--focus-ring`** | **`#a48fff`** | 5.4:1 or more on every surface (UI needs 3:1) |
| `--border` / `--border-strong` | `#2e2e2e` / `#3a3a3a` | decorative lines / input and control outlines |
| `--success` / `--error` / `--warning` | `#4caf50` / `#ef5350` / `#f5a524` | add `--warning`; the code uses `amber-400` ad hoc |
| Tailwind `gray` remap | `500 → #8a8a8a`, `400 → #a3a3a3` (neutral) | fixes the 143 uses of `text-gray-500` without touching files; it also removes the blue tint of Tailwind's cool grays (owner decision O3) |

**Type scale.** Keep the `--fs-*` tokens, and expose them as Tailwind sizes: `text-2xs` 10 px (uppercase eyebrows only), `text-caption` 11 px (meta, badges), `text-xs` 12 px (secondary UI), `text-sm` 14 px (body and controls), `text-base` 16 px (inputs on narrow, the modal caption on narrow), `text-lg` 18 px (section and page titles), `text-2xl` 24 px (the Settings title). Map 11.5 px to 11, 12.5 px to 12 or 13 by role, and 9 px to 10. **No new `text-[Npx]`.** Use one eyebrow recipe: `text-2xs font-medium uppercase tracking-wider text-muted`.

**Spacing.** Tailwind's 4 px grid is fine. The problem is control height, not spacing. Use 3 control heights: **28** (dense desktop, `h-7`), **36** (default, `h-9`), **44** (any control on narrow, `narrow:h-11`, and primary page actions). Gaps: 4 and 8 inside controls, 12 and 16 between groups, 24 between sections.

**Radii.** `rounded` 4 px (badges inside cards), `rounded-md` 6 px (buttons, inputs, menu items), `rounded-lg` 8 px (panels and popovers), `rounded-xl` 12 px (modals and sheets; sheets on narrow use `rounded-t-xl`), `rounded-full` (pills and avatars). Keep PostCard's `rounded-sm`: the tight tile is part of the identity. Drop `rounded-2xl` and the arbitrary 2 px and 4 px.

**Shadows.** Three: `shadow-pill` (`0 6px 20px -6px rgba(0,0,0,.6)`, the floating toolbar), `shadow-lg` (toasts, menus), `shadow-2xl` (modals, sheets).

**Icons.** 12 (inline meta), 14 (buttons and chips), 16 (toolbar, nav, icon buttons), 20 (BottomNav, sheet rows), 36 (empty states, stroke 1.5). Stroke width 2 at 16 px and under, 1.75 at 20, 1.5 at 36. The badge icons on cards stay as they are.

**z-index scale** (needs the portals of MOD-1, DLG-1 and Popover):

| Token | Value | Layer |
|---|---|---|
| `z-raised` | 10 | inside isolated cards and carousels |
| `z-toolbar` | 20 | floating gallery toolbar |
| `z-scrim` / `z-drawer` | 30 / 40 | menu drawer, filter sheet |
| `z-modal` | 50 | PostModal and dialogs |
| `z-modal-over` | 60 | lightbox, and dialogs opened from a modal |
| `z-popover` | 70 | menus and popovers |
| `z-toast` | 80 | toasts: an Undo shows above a modal |
| `z-critical` | 100 | ReauthDialog, DisclaimerGate |

### 3.2 Motion and `prefers-reduced-motion`

The motion system is good: tokens for easing and duration, `u-*` utilities, exit animations, and a blanket reduced-motion rule (`index.css:813-875`). Keep it. The gaps are in JavaScript, which the CSS rule can't reach:

| Gap | Fix |
|---|---|
| `DisclaimerGate.tsx:99` `scrollIntoView({ behavior: 'smooth' })` | `behavior: reduced ? 'auto' : 'smooth'` |
| Autoplaying post videos (`MediaCarousel.tsx:117`, `:218`) and hover video previews | Autoplay only without reduced motion; always `muted`. |
| `InfiniteCanvas.tsx:67-70` checks `matchMedia` inline | Use the shared hook. |
| No shared hook | `src/hooks/useReducedMotion.ts` (UX-2); every JavaScript animation and autoplay goes through it. |
| New sheets and drawers | Use `--dur-3` / `--ease-emphasized`, the existing curves; swipe-to-dismiss tracks the finger with no extra easing. |

### 3.3 Focus rings

**Today there is no visible focus anywhere.** `src/index.css:809-811` sets `:focus-visible { outline: none }`, recorded as an accepted trade-off. The probe tabbed 14 stops across the sidebar and toolbar: none had an outline or a ring. Only text inputs change their border color on focus. **SiteCard** (`SiteCard.tsx:79`) is the one component with a real ring.

Proposal (owner decision O1). `:focus-visible` only fires for keyboard and assistive-technology navigation, so mouse and touch users never see a ring, which removes the original reason to hide it:

```css
:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
```

- **Cards with `.u-clip-aa` need an inset ring.** The mask clips anything painted outside the border box, and an inset `box-shadow` is painted under the cover (GAL-4). Draw it on an overlay child: `focus-visible:[&>.focus-overlay]:ring-2`, or a `::after` on the card with `outline: 2px solid var(--focus-ring); outline-offset: -2px`.
- Inputs: keep the border-color change, and add the ring.
- Grid keyboard model (P2, later): Tab enters the grid once (roving `tabIndex`), the arrow keys move, Enter opens. Today every card is a tab stop (`PostCard.tsx:690`), so passing a 2,000-post library takes hundreds of Tab presses. Add a "Skip to posts" link before the toolbar.

### 3.4 Empty, loading and error patterns

The patterns are inconsistent: three loading idioms (skeleton, spinner with text, a bare logo), five empty-state looks (an icon with gray text, bare text, nothing at all: the AI panel returns `null` and the folder list renders nothing), and toasts implemented per view. The proposed primitives are in §3.6.

### 3.5 Accessibility, beyond focus and contrast

| Area | Finding | Fix |
|---|---|---|
| Dialogs | Of 12 overlays, 4 have `role="dialog"` with a label, 2 trap focus, none restores focus, none makes the background inert. | `useDialog()` (UX-2), adopted per lane. |
| Icon-only buttons | About 20 named by `title` only; 3 with no name (`ImportModal.tsx:71-76`, `AddBookmarkModal.tsx:361-366`, the tree chevrons). | `IconButton` requires `label`; set `aria-label` from the existing strings. |
| Live regions | Toasts, the selection count and the result count have no `aria-live`. | `ToastHost` uses `role="status"`. Add a visually hidden live region for "N posts" and "N selected". |
| Headings | The gallery has no `h1` (the other views have one). | A visually hidden `h1` with the current source or folder name. |
| `lang` | `web/index.html` is `lang="en"`, but the default language is Italian; it's corrected only after React runs (`i18n/index.tsx:251-257`). | Set `lang` early from the saved language (inline script or server). |
| Lists | The grid and the sidebar tree are plain `div`s. | `role="list"` and `role="listitem"` on the sidebar tree; grid semantics come with the roving model (P2). |
| Carousel dots | The active slide is shown by width and opacity only, with no `aria-current`. | Add `aria-current="true"` and `aria-label="Slide n of N"`. |

### 3.6 Shared primitives to add (UX-2)

All are small; budget them at **+4 KB gzip or less in the entry** (now 182.51 KB measured; the cap is 220).

| Primitive | Spec |
|---|---|
| `src/components/ui/IconButton.tsx` | `label` is required (it becomes `aria-label`, and `title` on desktop). Sizes `sm` 28, `md` 32 or 36, with `narrow:` 44. `tone` neutral or danger. `.u-press`. |
| `src/components/ui/Button.tsx` | `variant` primary (`--accent-fill`, white), secondary (border `--border-strong`), ghost, danger. `size` sm 28, md 36, lg 44, plus `narrow:min-h-11`. A loading state with an inline spinner. |
| `src/components/ui/EmptyState.tsx` | `icon` (36 px, stroke 1.5, `text-muted`), `title` (`text-sm font-medium text-primary`), `body` (`text-sm text-secondary max-w-xs`), `action` (Button secondary), `secondaryAction` (link). Centered in the space it's given; `u-fade-in-up`. |
| `src/components/ui/Notice.tsx` | Promotes `InlineNote` from `settings/ui.tsx`: tone info, ok, warn or error; error gets `role="alert"`. |
| `src/components/ui/Spinner.tsx` | Replaces the 24 ad hoc `Loader2` + `animate-spin` copies; `aria-label` with `role="status"` when standalone. |
| `src/components/ui/PageHeader.tsx` | A `leading` slot (the view passes UX-1's MenuButton, so UX-2 doesn't depend on UX-1), title, count or subtitle, actions; `h-14`, `px-4 narrow:px-3`. For Trash and Jobs. |
| `src/hooks/useToast.ts` and `src/components/ui/Toast.tsx` (`ToastHost`) | One region per view, `role="status" aria-live="polite"`, at most 3 stacked. Bottom center at `bottom-6` on desktop; on narrow at `bottom-[calc(64px_+_env(safe-area-inset-bottom))]`, above the BottomNav. `max-w-[min(92vw,420px)]`, wrapping text. Variants neutral, success and error (icon plus color, never color alone). Optional action (Undo) and a dismiss ×, 44 px on narrow. Duration 4 s, or 8 s with an action; pauses on hover and focus. `z-toast`. |
| `src/hooks/useDialog.ts` | Focus in (first focusable or `initialFocus`), trap, restore to the trigger, Escape, `inert` on `#root > :not(portal)`. Used by every modal, sheet and drawer. |
| `src/hooks/useReducedMotion.ts` | `matchMedia('(prefers-reduced-motion: reduce)')` with a change listener. |
| `Popover.tsx`: `presentation="auto"` | On narrow, menus open as a **bottom sheet** (`rounded-t-xl`, 48 px rows, a drag handle, a scrim, safe-area bottom padding); anchored on desktop. It also sets `role="menu"` focus handling. |

### 3.7 Copy and terminology (applies to every lane's own namespaces)

| Concept | EN | IT | Never in the UI |
|---|---|---|---|
| A saved item | post | post | item, element, elemento, reference |
| User-made group | folder | cartella | source, collection, board, connection |
| Everything saved | Library | Libreria | Bookmarks, Segnalibri (for the same thing) |
| Platforms | Instagram, X, Pinterest, Websites | Instagram, X, Pinterest, Siti web | "X / Twitter" (a transition label; owner may keep it until P5) |
| Delete | Delete moves to the trash; Delete forever | Elimina, Elimina definitivamente | |
| Session | Sign in, Sign out | Accedi, Esci | Log in |
| Background work | Jobs | Attività | Garbage collection, peer, token API, raw error codes |

- **Sentence case** for buttons and headings (the Downloads view uses Title Case).
- **No desktop-only instructions on the web** (GAL-2).
- **An i18n parity test** (UX-0): every key exists in both languages, and IT values that equal EN must be on an allowlist (brand names, "Post", "Account"). Known leaks: `filterBar.ts:15-16`; `settings.ts:30-35`; "source" untranslated 12× in `gallery.ts`, 10× in `collectionModal.ts`, 4× in `sidebar.ts`; the Italian model metadata from `electron/analyzer.ts` (SET-5). The manifest description lists TikTok, which Shelfy doesn't support (`web/public/manifest.webmanifest:6`).

---

## 4. Mobile pass (E8: "optimize the mobile UI")

Measured at 390×844 and 412×915 with touch emulation. The two widths behave the same.

| Area | Today | Target |
|---|---|---|
| **Layout at 390** | The toolbar overflows (GAL-1). The floating menu button covers content (SH-1). The column can be shifted sideways. In Settings the version pill crushes the subtitle (SET-1). | Each screen has one in-flow top row: MenuButton, title or search, and at most 2 icon actions. Nothing is fixed over content except toasts and sheets. The gallery column has `overflow-x: clip`. |
| **Touch targets ≥ 44 px** | BottomNav tabs and post prev/next pass. The sweep found under 44 px: drawer close 32, the menu button 40, sidebar rows about 32, tree chevrons 20, the search clear 20, Filters about 32, toolbar buttons 28, card checkboxes 20, modal header icons 32, menu rows about 32, Settings buttons about 28 and inputs 32, Jobs icons 24 to 28, auth buttons 40, ReauthDialog buttons 36. | One rule: every interactive element is `narrow:min-h-11 narrow:min-w-11`, or keeps its visual size and extends its hit area with a `.u-hit` utility (`::after { content: ''; position: absolute; inset: -8px }`). Card checkboxes keep their 20 px look with a 44 px hit area inside the card. List rows in the drawer and in sheets are 44 to 48 px tall. |
| **Thumb reach** | The menu sits top-left, the hardest spot to reach; selection actions and bulk menus sit at the top. | The menu stays top-left (the platform pattern, and in the flow now). Selection moves to a bottom bar (GAL-11). Menus open as bottom sheets. Filters open as a bottom sheet. |
| **Safe areas** | Only the BottomNav pads for the home indicator. | Top, left and right insets on the shell (SH-3). Fixed full-screen layers (PostModal, sheets, drawer, lightbox) pad themselves. Toasts sit above the BottomNav and its inset. Check in the iOS Simulator, both in Safari and as an installed PWA. |
| **BottomNav** | Library, Search, Jobs, Settings; 11 px labels; the inactive state fails contrast; Search does nothing distinct. | 12 px labels; inactive `--text-muted`; Search focuses the search field (SH-11). Whether Jobs stays a tab is owner decision O4; with P3's AI tab, five tabs is the limit. |
| **Gallery density** | 2 columns (183 px at 390, 198 px at 412), 8 px gaps. Text cards stay readable. Density can't be changed (the control is off-screen). | Keep 2 columns as the default. Offer 2 or 3 columns in the filter sheet's View section; at 3 columns (about 124 px) hide the hover overlay and keep the badges. |
| **Filters** | A 280 px push column that leaves the grid 110 px, with no scrim, no sheet behavior and native selects (`ios-04`). | A bottom sheet up to 85 dvh: a drag handle, sections (View; Library on narrow only; Media type; Download status; AI tags; the website facets when they apply), a sticky footer with [Reset] and [Show N posts], and swipe down to close. |
| **Post modal as a full-screen sheet** | Already full screen with stacked media and meta. But: the menu button floats above it (SH-1); two sets of arrows (MOD-5); 32 px header icons; no safe-area top; unmuted autoplay; broken-media fallback (MOD-2). | Header 56 px plus the safe area, with [×] [‹ ›] [folders] [⋯] at 44 px. Swipe left and right on the media for the previous and next post; swipe the carousel; dots with `aria-current`. Swipe down from the header to close (P2). `overscroll-behavior: contain` on its scroller. |
| **Gestures** | Long-press selects (no affordance); tap previews, then tap opens; the canvas pans and pinches. No swipe anywhere else; `touch-action` is set only on the canvas. | Add the swipes above, each with a visible alternative (a button). A one-time tip for long-press (GAL-12). `touch-action: pan-y` on horizontal swipe areas so vertical scrolling keeps working. |
| **Keyboard overlap** | Every input except `/device` is 12 to 14 px, so iOS zooms. There is no `visualViewport` handling and `enterKeyHint` is used nowhere. | `narrow:text-base` (16 px) on every input and textarea. `enterKeyHint` (search, send, done). On narrow, `scrollIntoView({ block: 'center' })` on focus inside sheets and the modal. Add `interactive-widget=resizes-content` to the viewport meta (Chrome for Android resizes the layout; iOS ignores it). |
| **Hover-only controls** | Sidebar folder edit, Activity dismiss, website hover controls. | Always visible on narrow, or available through long-press. |

---

## 5. Implementation plan

Each lane owns the files listed; no two lanes edit the same file. **Sequential** dependencies are listed where a lane uses another lane's new component. Every lane also:

- applies the copy glossary (§3.7) to the message namespaces it owns;
- replaces hardcoded greys and accents with the §3.1 tokens in the files it owns;
- keeps desktop (≥900 px) screenshots unchanged except for the findings it fixes;
- keeps `.u-clip-aa` on cards and adds no `backdrop-filter` per card;
- measures any scroll-path change with real wheel events (`web/e2e/perf-gallery.spec.ts`);
- uses `_` for spaces in Tailwind arbitrary `calc()`;
- keeps the entry at or under 220 KB gzip, with new views as `lazy(withMessages(...))`.

Sizes: S is about half a day of lane time, M about a day, L about two days.

| Order | Lane | Scope (finding IDs) | Owns | Needs | Model | Size |
|---|---|---|---|---|---|---|
| 1 | **UX-0 · Visual and accessibility harness** | A real-server spec that synthesizes a library and captures every §2 screen at the 4 viewports into `SHELFY_E2E_SHOTS`. Assertions: no horizontal overflow at 390 and 412 (no `scrollLeft` > 0 after focusing each toolbar control); listed controls inside the viewport and ≥44×44 on narrow; the modal on top (`elementFromPoint` at its header resolves inside `post-modal`); focus visible after Tab; a contrast check on key text nodes. Also the i18n parity test (§3.7). Assertions that depend on later fixes start as `test.fixme` and each lane enables its own. | new `web/e2e/server/ux.spec.ts`, new `tests/i18n-parity.test.ts`; additive helpers in `web/e2e/server/support.ts` | — | Sonnet | S |
| 2 | **UX-1 · Mobile shell** | SH-1, SH-2, SH-3 (shell part), SH-4 to SH-14, ST-1 (`ViewLoading` lives in `App.tsx`), ST-3 | `src/App.tsx`, `src/components/Sidebar.tsx`, `src/components/BottomNav.tsx`, new `src/components/MenuButton.tsx` (with `ShellContext`), `src/components/SourceIcon.tsx`, `src/i18n/messages/{nav,sidebar,app}.ts` | UX-2's `useDialog` (or ship its own and switch later) | **Opus** | M |
| 3 | **UX-2 · Tokens, contrast, focus and primitives** | §3.1 tokens and the Tailwind mapping, the gray remap (O3), `--text-muted`, `--accent-fill` (O2), the global `:focus-visible` (O1), `100dvh` on `#root` (SH-2), §3.6 primitives, `useDialog`, `useReducedMotion`, Popover `presentation="auto"`, the reduced-motion gaps of §3.2 in files it owns. Moves nothing out of the POST MODAL block of `index.css` (UX-5 owns lines 932-963). | `src/index.css` (except 932-963), `tailwind.config.ts`, `src/hooks/useToast.ts`, `src/components/Popover.tsx`, new `src/components/ui/*`, new `src/hooks/{useDialog,useReducedMotion}.ts`, `src/i18n/messages/{common,errors}.ts`, `web/index.html` (`lang`, `interactive-widget`), `web/public/manifest.webmanifest` | — | **Opus** | M |
| 4 | **UX-3 · Gallery: toolbar, filters, selection, states** | GAL-1, 2, 5, 6, 7, 9, 10, 11, 12, 13; §4 filter sheet and density; listens for `shelfy:focus-search` | `src/views/Gallery.tsx`, `src/components/FilterBar.tsx`, `src/components/FilterDrawer.tsx`, `src/components/GridSizeControl.tsx`, `src/components/VirtualPostGrid.tsx`, `src/components/PostGridSkeleton.tsx`, `src/i18n/messages/{gallery,filterBar,filterDrawer}.ts` | UX-1 (MenuButton), UX-2 (EmptyState, ToastHost, Popover sheet) | **Opus** | L |
| 5 | **UX-4 · Post cards** | GAL-3, GAL-4, GAL-8; the card focus ring (§3.3, inset); 44 px hit area for the card checkboxes; `alt` text | `src/components/PostCard.tsx`, `src/i18n/messages/postCard.ts` | UX-2 (tokens) | Sonnet | S |
| 6 | **UX-5 · Post modal as a sheet** | MOD-1 to MOD-13, the modal part of SH-3, and the lightbox | `src/components/PostModal.tsx`, `src/components/postmodal/*`, `src/components/ImageLightbox.tsx`, `src/index.css` lines 932-963 (POST MODAL block), `src/i18n/messages/{postModal,lightbox}.ts` | UX-2 (`useDialog`, IconButton, z tokens) | **Opus** | M |
| 7 | **UX-6 · Trash and Jobs** | TR-1 to TR-5, JOB-1 to JOB-4 | `src/views/Trash.tsx`, `src/views/Jobs.tsx`, `src/views/jobs/*`, `src/i18n/messages/{trash,jobs}.ts` | UX-1 (MenuButton), UX-2 (EmptyState, PageHeader, ToastHost) | Sonnet | S |
| 8 | **UX-7 · Settings** | SET-1 to SET-3 and SET-5 to SET-9 (SET-4 is in UX-8) | `src/views/Settings.tsx`, `src/views/settings/*`, `src/components/LanguageCard.tsx`, `src/i18n/messages/{settings,language}.ts` | UX-1, UX-2 | Sonnet | M |
| 9 | **UX-8 · Sign-in, re-auth, share and device** | AUTH-1 to AUTH-3, SET-4, SHR-1, SHR-2, DEV-1, ST-2 | `web/src/auth/*`, `web/src/share/*`, `web/src/Root.tsx`, `src/i18n/messages/{auth,share}.ts` | UX-2 | Sonnet | S |
| 10 | **UX-9 · Dialogs and folders** | DLG-1 to DLG-5; the Disclaimer gate's smooth scroll (§3.2) | `src/components/{CollectionModal,AddBookmarkModal,AddSiteModal,ImportModal,ImportFolderModal,FeedbackModal,DisclaimerGate}.tsx`, `src/i18n/messages/{collectionModal,addBookmark,addSite,importModal,importFolder,feedback,disclaimer}.ts` | UX-2 | Sonnet | M |
| 11 | **UX-10 · Desktop-only views and the token sweep** | DA-1, DA-2; token adoption in files no other lane owns (`src/views/{AiSearch,AiTags,AiTagsQueue,AiWebsites,Downloads,Browser}.tsx`, `src/views/websites/*`, `src/components/{ActivityCenter,RemoteAiBanner,Chip,InfiniteCanvas}.tsx`), with `useReducedMotion` in InfiniteCanvas | those files and their message namespaces | all others landed | Sonnet | M |

**Acceptance for every lane:**

1. UX-0's spec passes with the lane's `fixme`s turned on.
2. Before and after screenshots of the affected screens at the 4 viewports are attached to the lane report and reviewed by the lead. There is no pixel-diff tool in the repo; the lead compares them visually.
3. `pnpm typecheck`, the unit tests, the mocked web e2e and the desktop e2e pass.
4. The bundle report shows the entry at or under 220 KB gzip.

**Lane-specific checks:**

| Lane | Checks |
|---|---|
| UX-1 | At 390 and 412, no element overlaps the toolbar or the view headers. The drawer opens from MenuButton, closes with Escape and the scrim, and returns focus. Folders are reachable by Tab. At ≥900 px, screenshots are identical apart from SH-5 to SH-10. In the iOS Simulator (Safari and an installed PWA): the BottomNav is fully visible and nothing sits under the status bar. |
| UX-2 | Contrast table (§3.1) re-measured: every text token passes 4.5:1 on its surfaces. Tab shows a ring on sidebar rows, toolbar controls, inputs and menu items. Each primitive has a unit test; `useDialog` has tests for focus in, trap, restore and Escape. Entry +4 KB gzip at most. |
| UX-3 | At 390: every toolbar control reachable by tap; the column's `scrollLeft` stays 0 after focusing each control; the filter sheet opens and shows "Show N posts"; selection works from the Select button and from long-press; the bulk menu opens as a sheet. At 1024: the filter panel overlays and the cards keep their size. `perf-gallery.spec.ts` with real wheel events shows no fps regression. |
| UX-4 | Zoomed screenshots of a fallback card hovered and not hovered: no overlapping text. Selected cards show the ring on image cards and on fallback cards. The keyboard ring is visible and not clipped by `.u-clip-aa`. |
| UX-5 | The modal stays on top of every shell element. A failed image shows the fallback, not the broken glyph (test with a 404 media URL). Text posts have no empty media pane. On narrow: header controls are 44 px, swiping between posts works, the safe area is respected. Videos start muted. Focus returns to the card. |
| UX-6 | Empty Trash and Jobs use EmptyState. Queues appear only with jobs. Toasts sit above the BottomNav. |
| UX-7 | Settings at 390 in EN and IT: no clipped text and no overflow. One Sign out. A storage bar without a quota doesn't look full. The desktop model cards are in English. |
| UX-8 | Re-auth with no passkey shows email as primary. Auth inputs don't zoom on iOS. The card doesn't jump when the error appears. |
| UX-9 | Every dialog passes the `useDialog` tests; axe-style checks show labels associated with their fields. |
| UX-10 | The token sweep leaves no failing grey in the touched files. Desktop e2e green. |

**Parallelism:** UX-0 and UX-2 can start at once, and UX-1 can start with them. Once UX-1 and UX-2 land, UX-3, UX-4, UX-5, UX-7, UX-8 and UX-9 can run in parallel (their files don't overlap), with UX-6 alongside. UX-10 goes last.

---

## 6. What not to change

The review recommends no redesign. Keep:

- **The identity:** dark-only canvas `#0f0f0f`; violet `#7b5cff` (O2 touches only the fill behind white labels); Space Grotesk for the wordmark and headings; the SHELFY wordmark with the bookmark logo; the restrained, quiet chrome that lets the references carry the color.
- **The floating "island" toolbar** over the grid (Apple Maps style), with `backdrop-blur` on at most 3 pills. **Never** blur per card.
- **The grid:** square tiles with tight gaps, `rounded-sm`, media zoom inside the tile on hover with no lift, the platform badge bottom-left and the media-type badge bottom-right, text cards with the quote mark and the "TEXT" eyebrow, the gradient overlay on hover, `.u-clip-aa`, and the overlay's `-inset-2` trick.
- **The motion system** (tokens and `u-*` utilities) and the reduced-motion safety net.
- **The navigation model:** a sidebar tree of platforms, folders, Trash and Jobs with Settings at the bottom; a drawer on narrow; the BottomNav on narrow.
- **The selection model:** Select → checkboxes → a contextual bar ("N selected · Select all · Actions ▾ · ×") in place of the toolbar. On narrow only its position changes (GAL-11).
- **The post modal:** media and a 380 px meta column on desktop, stacked on narrow; post arrows outside the modal on desktop.
- **The auth pages:** a centered card with the wordmark, a magic link spent only on click, `/device`'s monospace code and warning.
- **Settings:** a tab per section on the web; cards with an icon, title and description; a two-step confirm for destructive actions.
- **The tone of the copy:** short, honest, sentence case, bilingual.
- **Performance:** virtualized grid, lazy views (`lazy(withMessages(...))`), and the entry budget.

---

## 7. Owner decisions

| # | Decision | Recommendation | If no answer |
|---|---|---|---|
| O1 | **Show keyboard focus rings again.** `index.css` records hiding them as an accepted trade-off. `:focus-visible` never shows for mouse or touch. | Yes | UX-2 ships the ring (WCAG 2.4.7 is level AA). |
| O2 | **Darken the accent *fill* behind white labels** from `#7b5cff` to `#6d4dff` (4.36 to 5.05:1). Accent text and indicators move to `#a48fff`. | Yes; the change is barely visible. | Keep `#7b5cff`, and make primary labels 16 px semibold on narrow only. |
| O3 | **Remap Tailwind's `gray` to neutral and accessible values** (`500` to `#8a8a8a`, `400` to `#a3a3a3`). Secondary text gets slightly brighter and loses its blue tint. | Yes | Each lane replaces `text-gray-500` in its own files; slower and less uniform. |
| O4 | **BottomNav tabs.** Keep Jobs as a tab, or move it to the drawer to make room for AI (P3)? Should Search focus the search field? | Move Jobs to the drawer when AI arrives. Yes to Search. | Jobs stays; Search focuses search. |
| O5 | **Filters on desktop:** remove the "Bookmarks" source list from the filter panel while the sidebar is visible. | Yes | Rename it "Library" and keep it. |
| O6 | **Website facets:** show Industry and Site type only for Websites. | Yes | Keep them always, but restyle them. |
| O7 | **The version pill:** move it from the Settings header to Legal → About. | Yes | Keep it in the header on desktop and hide it on narrow. |
| O8 | **On narrow, the selection bar moves to the bottom** (replacing the BottomNav while selecting), and filters and menus open as bottom sheets. | Yes (E8) | — |
| O9 | **The re-auth operator command** (E4: the owner is the operator): keep it, behind a disclosure? | Behind a disclosure | Unchanged, but collapsed. |

## Appendix A: screenshots

In `/Users/fant/work/experiments/shelfy-web-local/ux-audit/shots/`, not committed. Prefixes `desk` (1440×900), `tab` (1024×768), `ios` (390×844, touch), `android` (412×915, touch), `electron` (desktop app, 1440×900), `p2-*` (menus, re-auth, share, Italian), `probe-*` (the overflow probe).

| # | Screen |
|---|---|
| 00 | magic-link page, consent gate (desk only) |
| 01 / 01b | sign-in / sign-in with an invalid link |
| 02 / 02b | gallery / gallery after 6 Tab presses (no focus visible) |
| 03 | gallery after a wheel scroll (hover overlay over a fallback card) |
| 04 | filter panel |
| 05a / 05b / 05c | the Search tab (narrow) / no results / results |
| 06 | selection and bulk bar |
| 07 / 07b / 07c | post modal / scrolled (narrow) / text-only post |
| 08 | menu drawer (narrow) |
| 09 | folder `/c/1` |
| 10 / 11 | Trash (empty) / Jobs |
| 12-* | Settings: account, language, storage, legal |
| 13 / 14 | `/device` / 404 |

The scripts that made them are next to the screenshots: `shots.mjs`, `probe.mjs`, `probe2.mjs`, `electron-shots.mts`, and `srv.sh`, which runs the server with this audit's environment.
