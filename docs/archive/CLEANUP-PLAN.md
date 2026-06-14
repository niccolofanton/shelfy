> **Documento storico / archiviato.** Piano in gran parte completato, conservato
> come riferimento. Non riflette necessariamente lo stato attuale del codice. Per
> l'architettura corrente vedi [docs/architecture.md](../architecture.md).

# Piano di pulizia e refactor

Obiettivo: rimuovere duplicazioni verificate e un bug funzionale, senza cambiare
comportamento (a parte il fix). Tutte le voci qui sotto sono state **confermate
nel codice** (riferimenti `file:riga`). Si lavora a batch, dal più sicuro al più
invasivo, con commit locale ad ogni batch.

> Nota: l'albero di lavoro contiene già lavoro in corso non correlato (dictation/STT
> in `electron/stt.js`, `electron/serverUtils.js`, `src/hooks/useDictation.js`,
> `src/lib/dictation/`; mind map in `src/views/MindMapCanvas.jsx`,
> `src/components/TagMindMap.jsx`). I riferimenti di riga riflettono lo stato
> attuale (con quel lavoro presente) e potrebbero spostarsi. I nuovi file vanno
> inclusi nelle consolidazioni dove pertinente (es. `TagMindMap.jsx` ridefinisce `ACCENT`).

---

## Batch 0 — Bugfix (rischio minimo, ~2 righe)

**Bug: "Promuovi in collection" da AiTags e AiSearch fallisce sempre.**
- `preload.js:21` espone `createCollection: (name, color)` (posizionale).
- `src/views/AiTags.jsx:503` e `src/views/AiSearch.jsx:223` lo chiamano con un
  oggetto `createCollection({ name, color: ACCENT })` → in `db.js:1697`
  `(name ?? '').trim()` riceve un oggetto e lancia `TypeError`.
- **Fix:** allineare le due chiamate alla forma posizionale
  `createCollection(name, ACCENT)`. (In alternativa instradarle tramite
  l'hook `useCollections`, che è già corretto — `useCollections.js:19`.)
- **Verifica:** creare una collection da AiTags e da AiSearch e confermare che
  i post vengano aggiunti senza il toast di errore.

---

## Batch 1 — `src/lib/` condiviso per il frontend (rischio basso)

Il `lib/` è quasi vuoto (solo `asset.js`): è la causa della proliferazione di
costanti e helper copia-incollati. Creare:

### `src/lib/theme.js`
- `ACCENT = '#7B5CFF'` (oggi ridefinito 3×: `TagMindMap.jsx:9`, `AiSearch.jsx:11`,
  `AiTags.jsx:12`) ed eventuale `ACCENT_HOVER`.
- Sostituire le ~73 occorrenze literal `#7B5CFF`/`#5A3DDE` sparse in 12 file
  (dove sono inline-style; per le `className` Tailwind valutare la CSS var
  `--accent` già presente in `index.css`).

### `src/lib/platform.js`
- Mappa unica `{ instagram, twitter } → { label, color, dotClass }` e un
  componente `<PlatformIcon platform variant />`.
- Rimpiazza: `bg-pink-500`/`bg-blue-500` (`Sidebar.jsx:18-24`, `Settings.jsx:7-8`),
  `#c2185b`/`#1565c0` (`PostModal.jsx:453`), `#e1306c`/`#1da1f2` (`Downloads.jsx:15-16`),
  e i due `PlatformIcon` distinti (SVG in `PostCard.jsx:5-21` vs badge testuale
  IG/TW in `Downloads.jsx:9-23`). Label `'X / Twitter'` ripetuta.

### `src/lib/format.js`
- `formatDate(ts, { time })` che unifica i due `formatTimestamp` divergenti
  (`PostCard.jsx:72` solo data + try/catch; `PostModal.jsx:33` data+ora senza).
- Spostare anche `formatCount`/`formatBadge` (`Sidebar.jsx:27-35`) e
  `formatElapsed` (`Browser.jsx:72`).

### `src/lib/exportPosts.js`
- `copyPostLinks(posts)` ed `exportPostsMarkdown(posts, name)` per de-duplicare
  `handleCopyLinks`/`handleExportMarkdown` (`AiTags.jsx:459/470` ≈ `AiSearch.jsx:215/226`)
  e la variante clipboard di `Gallery.jsx:242`.

**Verifica batch 1:** avviare l'app, controllare colori/icone piattaforma nelle
viste, date nei card/modal, e i pulsanti copia-link/export markdown.

---

## Batch 2 — Componenti condivisi duplicati (rischio basso)

- **`Toast`**: identico (tranne `data-testid`) in `AiSearch.jsx:24` e
  `AiTags.jsx:966` → estrarre `src/components/Toast.jsx` con prop `data-testid`.
- **`Chip`**: `AiTags.jsx:30` ridefinisce localmente una versione che è un
  sottoinsieme di `components/Chip.jsx` (quello condiviso è un superset con
  `className`/`data-testid`) → eliminare il locale e importare il condiviso.

**Verifica batch 2:** i toast continuano ad apparire (test e2e usano i
`data-testid`), i chip si renderizzano identici.

---

## Batch 3 — Hook frontend: estrazione pattern (rischio medio)

- **`useTransientMessage()`**: timer toast `useRef`+`setTimeout(3000)` ripetuto
  (AiTags, AiSearch, Gallery).
- **`useLatestRequest` / `useAbortableFetch`**: unifica il guard anti-race, che
  oggi ha **due varianti** — contatore numerico `reqId` (`useAiSearch.js:90/114`,
  `AiTags.jsx:295`, `Gallery.jsx:73`) e token di abort `{ aborted:false }`
  (`usePosts.js:10-67`). Sceglierne una.
- **`useIpcSubscription(subscribe, handler)` + `upsertByKey(list, job)`**: il
  boilerplate subscribe→cleanup su `onAnalyzeProgress`/`onDownloadProgress` è 3×
  (`useDownloads.js`, `useAnalysis.js`, `useAiTags.js`); il reducer upsert-by-key
  vero e proprio è 2× (`useDownloads.js:30`, `useAnalysis.js:31` — **non** in
  useAiTags, che fa invece `load({ silent:true })` con debounce).
- **`withReload(fn)`**: collassa il pattern `mutate → load({silent}) → return res`
  ripetuto in `useAiTags.js` (108/114/131/140…).

**Verifica batch 3:** download e analisi aggiornano la UI in tempo reale; le
mutazioni dei tag ricaricano gli aggregati; nessuna race nelle ricerche.

---

## Batch 4 — Main process (rischio medio, alto valore)

### `electron/parser-common.js`
- `normalizeAiFields` è **byte-identico** in `ig-parser.js:274` e `tw-parser.js:176`.
- `normalizeMediaList` differisce solo per il guard `&& post.mediaType !== 'text'`
  (tw) → parametrizzare.

### `electron/job-queue.js` (factory)
- `downloader.js` e `analyzer.js` reimplementano la stessa coda: stato
  `jobsMap`/`postCache`/`pendingQueue`/`abortMap`/`runningCount`/`onJobUpdate`,
  più `setJob`/`patchJob` (byte-identici), `pumpQueue`, `cancelJob`, `cancelAll`,
  `retryJob`, `clearCompleted`, `getJobs`, `setProgressEmitter`.
- Factory che riceve `runJob` e `CONCURRENCY`; il downloader aggiunge sopra il
  pezzo `pauseAll`/`resumeAll`/`pausedKeys`/`isPaused`.
- `detectPlatform` (identico) e `firstExisting` (equivalente) vanno nel modulo
  condiviso.

### `electron/preload.js`
- I 6 `onXxx(cb)` (righe 101-130) differiscono solo per il canale →
  `const subscribe = (channel) => (cb) => { ... }`.

**Verifica batch 4:** la suite test del main process resta verde (`npx vitest run tests/`),
download/analisi/pausa funzionano end-to-end. Attenzione: le modifiche al main
process richiedono restart di `npm run dev` (no hot-reload).

---

## Ordine e checkpoint

1. Batch 0 (bugfix) → commit.
2. Batch 1 (lib frontend) → commit.
3. Batch 2 (Toast/Chip) → commit.
4. Batch 3 (hook) → commit.
5. Batch 4 (main process) → commit.

Ogni batch: niente cambi di comportamento (salvo Batch 0), test verdi prima del
commit, commit locale isolato così ogni passo è reversibile.
