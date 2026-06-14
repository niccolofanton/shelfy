> **Documento storico / archiviato.** Piano in gran parte completato, conservato
> come riferimento. Non riflette necessariamente lo stato attuale del codice. Per
> l'architettura corrente vedi [docs/architecture.md](../architecture.md).

# Piano — Revisione tag AI & ricerca

Obiettivo: rendere la categorizzazione AI specifica per **reference di sviluppo / arte / creative coding**
(tecnologie e terminologie), eliminare le categorie generiche, e migliorare ricerca e modifica.

Decisioni prese con l'utente:
- **Tassonomia**: niente categorie fisse. Solo **tag ricchi e specifici** + **entità/strumenti** (librerie,
  framework, software, studi, persone). Si rimuovono gli assi `category` e `content_type` dal flusso AI.
- **Hover**: aprire all'hover il **pannello Filtri** della gallery e i **menù/azioni della schermata AI Tags**
  (i veri *modal*, es. merge/rinomina, restano a click — non si convertono in hover).

---

## Contratto API condiviso (definito a monte, per parallelizzare)

- `analyzer.expandSearchQuery(query, signal?) => Promise<{ tags: string[], keywords: string[] }>`
  - inferenza **solo testo** sul modello locale; se il modello non è pronto → lancia/ritorna vuoto.
- L'output di `analyzeFrames` **non** include più `category`/`contentType`.
  `db.updateAiAnalysis` continua a tollerare quelle chiavi se `undefined` (nessun break).
- Nuove preload API:
  - `updatePostAiAnalysis(id, fields) -> invoke('analyze:updateManual', { id, fields })`
  - `suggestSearch(query) -> invoke('search:suggest', { query })` → `{ tags: string[], posts: Post[] }`
- Nuovi handler IPC: `analyze:updateManual`, `search:suggest`.

---

## Task 1 — Prompt & schema specifici per reference creative-tech  _(Agente A · electron/analyzer.js)_
- Rimuovere `CATEGORIES` e `CONTENT_TYPES` (e l'asse category/content_type dallo schema e da `analyzeFrames`).
  `getTaxonomy` ritorna `{ categories: [], contentTypes: [] }` (compat, nessun crash a valle).
- Riscrivere `SYSTEM_PROMPT` + `buildUserPrompt`: catalogare reference per uno **sviluppatore/artista/creative coder**.
  - Tag: tecnologie/tecniche/stili **specifici** (es. `three.js`, `glsl`, `raymarching`, `p5.js`,
    `touchdesigner`, `blender geometry nodes`, `particle system`, `webgl`, `instancing`, `shader`, `wgsl`,
    `houdini`, `signed distance field`, `generative`, `data viz`, `creative coding`...).
  - Entità = **strumenti/librerie/framework/software/persone/studi** nella loro forma originale.
  - Vietato generalizzare: niente tag-ombrello tipo "altro", "vario", "contenuto".
- `RESPONSE_FORMAT`: `{ description, tags, entities, search_keywords, save_reason, language }`.
- **Task 4 (incluso qui)**: nessun valore "altro"/"Altro"; il fallback diventa lista vuota / `null`, mai un bucket generico.
- `runJob` scrive via `db.updateAiAnalysis` senza category/contentType.
- Aggiungere ed esportare `expandSearchQuery`.

## Task 2/3/6/7 backend — DB, IPC, preload, parser  _(Agente B · db.js, ipc.js, preload.js, ig-parser.js, tw-parser.js)_
- **Import/export con dati AI (Task 3)**:
  - `exportAllPosts`/`rowToPost` già includono i campi `ai*` → verificare che l'export li contenga.
  - I parser `normalizeExportedPost` (IG/TW) devono **propagare** i campi AI se presenti nel JSON
    (`aiDescription, aiTags, aiCategory, aiContentType, aiEntities, aiKeywords, aiLanguage, aiSaveReason,
    aiStatus, aiModel, aiAnalyzedAt`).
  - `bulkUpsert` applica i campi AI importati riusando la sync derivata (`post_tags`/`post_entities`):
    per i post **nuovi** sempre; per i **già esistenti** solo se non hanno ancora `ai_status` (non sovrascrivere analisi locali).
- **Modifica manuale analisi (Task 6 backend)**: handler `analyze:updateManual` → `db.updateAiAnalysis(id, fields)`
  con `status:'done'` e `model:'manuale'`. Preload `updatePostAiAnalysis`.
- **Ricerca AI (Task 7 backend)**: handler `search:suggest` → `analyzer.expandSearchQuery(query)` →
  `db.getPostIdsByTags(tags,'or')` + `db.getPostsByIds(...)` (limit ~40). Ritorna `{ tags, posts }`.
  Preload `suggestSearch`.
- Lasciare `category`/`contentType` in `buildPostFilter` invariati (innocui; la UI non li userà più).

## Task 2/4/5/8 frontend AI Tags + Filtri  _(Agente C · views/AiTags.jsx, hooks/useAiTags.js, components/FilterBar.jsx)_
- **Rimuovere il tag cloud (Task 2)**: `CloudTag`, la sezione "Tag cloud", il `useMemo` `cloud`.
- **Aggiornamento real-time (Task 5)**: `useAiTags` si sottoscrive a `onAnalyzeProgress`; su job `done`/cambi stato
  esegue `load()` con debounce (~800ms). La tab riflette tag/coverage durante l'analisi.
- **Niente categorie (Task 4)**: rimuovere i pannelli "Categorie" e "Tipo di contenuto" dalla Dashboard
  e i relativi filtri/stati `category`/`contentType` in AiTags.
- **FilterBar**: rimuovere le select Categoria e Tipo contenuto (e la chiamata `getTaxonomy`).
  Aprire il **pannello Filtri all'hover** (mouseenter/leave su bottone+pannello, con piccolo delay di chiusura).
- **Hover AI Tags (Task 8)**: i menù/dropdown popover della schermata si aprono all'hover; i modal restano a click.

## Task 6/7/8 frontend Gallery + dettaglio  _(Agente D · views/Gallery.jsx, components/PostModal.jsx, hooks/useAnalysis.js)_
- **Ricerca AI suggerita (Task 7)**: la ricerca testuale già cerca in descrizione+tag.
  In più, su query non vuota, chiamare (debounced, **dopo** i risultati testuali) `suggestSearch(query)`.
  Mostrare una sezione separata **"Suggeriti dall'AI"** sotto i risultati normali:
  - visivamente distinta (accento viola + badge sparkle), con i **tag suggeriti dall'AI** mostrati come chip;
  - esclude i post già presenti nei risultati testuali; arriva leggermente dopo (indicatore di caricamento);
  - gestione silenziosa se il modello non è pronto/errore.
- **Rigenera + modifica manuale (Task 6)**: nel `PostModal`/`AiPanel`
  - bottone **"Rigenera analisi"** (ri-accoda l'analisi);
  - modalità **modifica manuale** di descrizione e tag (e save_reason): salva via `updatePostAiAnalysis`.
- **Hover (Task 8)**: il popover "Aggiungi a source" resta a click *(non richiesto dall'utente)*;
  applicare hover solo dove indicato dall'utente (Filtri + AI Tags, gestiti dall'Agente C).
- `useAnalysis.js`: wrapper opzionale `updatePostAiAnalysis`.

---

## Verifica & commit
- Per file Electron (CommonJS): `node --check <file>`.
- Frontend: `npx vite build`.
- Test unit: `npx vitest run tests/` (escludere e2e). Attenzione ai fallimenti pre-esistenti noti
  (downloader/useDownloads stale, db.test ABI nativo) — non sono regressioni.
- Commit locale al termine di ogni milestone.
