# Contribuire a Shelfy

Grazie per l'interesse. Questa guida copre prerequisiti, flusso di sviluppo,
comandi di qualità e convenzioni. Per capire com'è fatta l'app vedi
[docs/architecture.md](docs/architecture.md); per build/rilascio Windows vedi
[docs/windows.md](docs/windows.md).

> Shelfy è rilasciato sotto **[Apache License 2.0](LICENSE)**. Contribuendo accetti che
> il tuo contributo sia distribuito sotto la stessa licenza (modello _inbound = outbound_);
> aggiungi un trailer `Signed-off-by:` ai commit (DCO, `git commit -s`).

---

## Prerequisiti

- **Node.js 22 o più recente** e **npm 10+** (vincolati in `package.json` →
  `engines`).
- `ffmpeg` è bundle (`ffmpeg-static`). Per le funzioni AI/download servono, a
  runtime, `yt-dlp` e un `llama-server` (`llama.cpp`, build **b9370**); in dev
  su macOS:
  ```bash
  brew install yt-dlp
  # llama.cpp b9370 → metti llama-server in .vlm/llama-b9370/llama-server
  ```
  Override dei percorsi via `YTDLP_BIN`, `FFMPEG_BIN`, `LLAMA_SERVER_BIN`.

## Setup

```bash
npm ci                # install riproducibile da package-lock
npx electron-rebuild  # ricompila better-sqlite3 per l'ABI di Electron
```

> `electron-rebuild` è necessario perché `better-sqlite3` è un modulo nativo che
> deve combaciare con l'ABI di Electron, non con quella di Node.

## Flusso di sviluppo

```bash
npm run dev    # Vite (renderer) + Electron con hot reload
```

- Il **renderer** (`src/`) ha hot-reload via Vite.
- Il **main process** (`electron/`) viene **riavviato automaticamente** da
  `electronmon` su modifiche a `electron/**`. Se il backend sembra non cambiare,
  controlla nel log che il restart sia avvenuto.

I file in `electron/` sono **CommonJS**; per una validazione sintattica rapida:
`node --check electron/<file>.js`.

## Comandi di qualità

Prima di proporre una modifica, fai girare:

```bash
npm run lint        # ESLint
npm run format      # Prettier (scrive); format:check per solo verifica
npm run typecheck   # tsc --noEmit (type-check su JSDoc/TS)
npm run test:run    # unit test (Vitest, single run)
npm run test:e2e    # end-to-end (Playwright)
```

Note utili:
- `npm run lint:fix` applica i fix automatici di ESLint.
- Per i soli unit test escludendo gli e2e: `npx vitest run tests/`.
- `npm test` avvia Vitest in **watch**; `npm run coverage` con copertura.
- La suite unit è verde; alcuni test sono **saltati di proposito** quando manca un
  modulo nativo (es. `better-sqlite3` non ricompilato per l'ABI corrente): non
  scambiare gli `skip` per regressioni.

## Convenzioni di commit

Il progetto usa **[Conventional Commits](https://www.conventionalcommits.org/)**
(già in uso in tutta la history), in **italiano**:

```
<tipo>(<scope>): <descrizione>
```

Tipi comuni: `feat`, `fix`, `perf`, `refactor`, `chore`, `docs`, `test`.
Esempi reali dal repo:

```
feat(updater): self-rebuild Windows + binari sidecar da upstream
fix(ai-tags): elimina il flicker del pannello AI durante l'analisi
perf(gallery): scroll nativo fluido — rimosso backdrop-filter dai chip
docs(win): guida self-rebuild
```

I commit di rilascio seguono `chore(release): vX.Y.Z`. Da questi messaggi è
generabile il [CHANGELOG](CHANGELOG.md).

## Branch flow

- `main` — ramo di default, stabile.
- `dev` — integrazione del lavoro in corso (le release partono da qui).
- branch di feature/fix dedicati (es. `feat/web-references-poc`,
  `fix/swarm-findings`), poi merge.

Non rilasciare con un **working tree sporco**: il source zip del self-rebuild
Windows è prodotto da `git archive HEAD` (solo file committati), quindi eventuali
modifiche non committate **non** finiscono nel pacchetto pubblicato dalla CI.

## Dove vivono i dati

Tutto è local-first, sotto la `userData` dell'app:

- `shelfy.sqlite` — database (WAL).
- `Partitions/social/` — sessione webview persistita (cookie login IG/X):
  trattala come un credential store.
- `runtime-bin/` — binari sidecar scaricati (yt-dlp/ffmpeg/llama/whisper).
- `models/` — pesi dei modelli AI (VLM ~3 GB, embedder, whisper).
- `logs/main.log` — log del main process.

Dettagli e percorsi in [docs/architecture.md → Dati e sessioni](docs/architecture.md#dati-e-sessioni).

## Test e harness AI

Gli harness di valutazione per la qualità AI hanno README dedicati:

- [scripts/search-eval](scripts/search-eval/README.md) — retrieval della ricerca.
- [scripts/extract-eval](scripts/extract-eval/README.md) — estrazione tag/keyword.
- [scripts/cluster-eval](scripts/cluster-eval/README.md) — clustering dei tag.
