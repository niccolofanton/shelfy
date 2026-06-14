# Changelog

Tutte le modifiche degne di nota a questo progetto sono documentate qui.

Il formato segue [Keep a Changelog](https://keepachangelog.com/it/1.1.0/) e il
progetto aderisce al [Versionamento Semantico](https://semver.org/lang/it/).

> Le voci sono ricostruite dai tag git e dai messaggi di commit
> ([Conventional Commits](https://www.conventionalcommits.org/)). Una versione
> futura può essere generata automaticamente dalla history (es.
> `feat:`/`fix:`/`perf:` → Aggiunto/Corretto/Modificato).

## [Unreleased]

### Aggiunto
- Siti web come reference (`platform='web'`): voce "Aggiungi sito" nel sottomenu
  Browser, source "Siti web" non editabile nel sidemenu, avanzamento cattura nel
  Centro Attività, pannello AI "Websites" con dettaglio scansione in tempo reale.
- Fondamenta di qualità statica: ESLint, Prettier, typecheck e igiene del repo.
- Documentazione strutturata: cartella `docs/` (indice, `windows.md`,
  `architecture.md`), `CONTRIBUTING.md`, questo `CHANGELOG.md`.

### Corretto
- Cattura siti: la finestra offscreen viene ora composita, gli screenshot
  funzionano; timeout su `eval` in-page e `capturePage` per robustezza.

## [1.3.6] - 2026-05-31

### Aggiunto
- Web references (POC siti web): fondamenta DB + `net-safety` (SSRF guard
  condivisa), moduli backend di cattura/arricchimento/AI, orchestrazione async,
  frontend (card, modale, filtro source). _(funzionalità additiva/dormiente fino
  al wiring completo)_

### Corretto
- macOS: firma ad-hoc reale tramite hook `afterSign`.

## [1.3.5] - 2026-05-31

### Modificato
- Gallery: rimosse le azioni bulk "Esporta link" e "Seleziona caricati".
- Build: hardening di download/provisioning + firma ad-hoc macOS.

## [1.3.4] - 2026-05-31

### Aggiunto
- Cross-fade keep-alive delle viste e rifiniture di componenti/viste.
- Import/export: round-trip delle collezioni (cartelle, incluse quelle Instagram).
- Windows: `install-shelfy.ps1`, bootstrap one-liner (Node + build + install).

### Corretto
- Import/export: round-trip dei tag AI + tier; aggiornamento dei post esistenti.

## [1.3.3] - 2026-05-31

### Modificato
- Updater: gli aggiornamenti di progresso del download sono limitati a 1 per
  percento (evita il flooding IPC e i freeze UI).

### Corretto
- Centro Attività: riflette correttamente lo stato "in pausa".

## [1.3.2] - 2026-05-31

### Modificato
- Release: `publish-r2` carica solo gli artefatti cambiati (risparmio banda).

### Corretto
- AI Tags: eliminato il flicker del pannello AI durante l'analisi in corso.

## [1.3.1] - 2026-05-31

### Corretto
- Release: rimossi artefatti scratch dal repo (i symlink rompevano il
  self-rebuild Windows).

## [1.3.0] - 2026-05-31

### Aggiunto
- Tag alias: proposte LLM accettabili/rifiutabili + harness di eval del
  clustering (`cluster-eval`).

## [1.2.0] - 2026-05-31

### Aggiunto
- Tuning del backend AI adattivo all'hardware (RAM/core/GPU/VRAM) con override in
  Settings e fallback variante GPU→CPU.
- Toggle Automatico/Personalizzato nella card Prestazioni.
- Embeddings di testo locali (e5-small) per il clustering tag.
- Centro Attività unificato in sidebar.
- macOS: dopo aver aperto il `.dmg`, l'app chiede conferma e si chiude per
  consentire la sostituzione.

### Modificato
- Performance: virtualizzazione di tutte le griglie di post (`VirtualPostGrid`),
  keep-alive delle viste, skeleton di caricamento e aggiornamenti ottimistici.

### Corretto
- Updater: detection robusta di Node ≥ 20 (fnm/nvm/volta/scoop) + `node-probe.log`;
  fix `spawn powershell ENOENT` (chiave `Path` corretta, powershell assoluto).
- Correzioni di bug e performance emerse da review multi-agente.

## [1.1.1] - 2026-05-30

### Aggiunto
- Updater Windows **self-rebuild**: si pubblica solo il source, il client
  ricompila l'installer; binari sidecar scaricati a runtime da upstream.

### Documentazione
- Guida Windows self-rebuild; fix della versione in `source.json` con
  `build-windows.ps1 -Publish`.

## [1.0.2] - 2026-05-30

### Aggiunto
- macOS: download in-app del `.dmg` con progress, poi apertura.
- Updater: notifica non invadente, check ogni 60s, riavvio rifiutabile.
- Binari sidecar fuori dall'installer, scaricati a runtime.
- Windows: flag `-Version` per build con versione esplicita.

### Corretto
- Updater: canale Nightly mappato ad `alpha` (gerarchia nativa electron-updater).

## [1.0.0] - 2026-05-29

Prima release. Archiviazione locale dei post salvati da Instagram e X.

### Aggiunto
- Cattura dei post salvati via webview loggato (`persist:social`) con
  intercettazione delle API della piattaforma; parser IG/X → `Post` canonico.
- Libreria & collezioni: gallery infinite-scroll, filtri faceted, ricerca su
  caption/autori/descrizioni/tag, collezioni colorate.
- Downloader media (yt-dlp + ffmpeg) con coda pausabile/retriabile.
- Tagging AI on-device con VLM locale (Qwen3-VL via llama.cpp): descrizione,
  tag, entità, keyword di ricerca, "save reason".
- Dashboard AI Tags: frequenza, co-occorrenza, clustering, suggerimenti di
  merge, rinomina/merge one-click.
- AI Search: ricerca conversazionale (NL → tag + keyword) con gallery suggerita;
  harness `search-eval` ed `extract-eval` per il tuning del retrieval.
- Auto-update OTA via feed Cloudflare R2 con canali stable/nightly e script di
  rilascio; build Windows cross-platform e file logger.

### Sicurezza
- Hardening dei webContents del webview, permission allow-list, CSP,
  `windowOpenHandler`; guard SSRF e host allowlist per media/modelli.

[Unreleased]: https://github.com/your-org/shelfy/compare/v1.3.6...HEAD
[1.3.6]: https://github.com/your-org/shelfy/compare/v1.3.5...v1.3.6
[1.3.5]: https://github.com/your-org/shelfy/compare/v1.3.4...v1.3.5
[1.3.4]: https://github.com/your-org/shelfy/compare/v1.3.3...v1.3.4
[1.3.3]: https://github.com/your-org/shelfy/compare/v1.3.2...v1.3.3
[1.3.2]: https://github.com/your-org/shelfy/compare/v1.3.1...v1.3.2
[1.3.1]: https://github.com/your-org/shelfy/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/your-org/shelfy/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/your-org/shelfy/compare/v1.1.1...v1.2.0
[1.1.1]: https://github.com/your-org/shelfy/compare/v1.0.2...v1.1.1
[1.0.2]: https://github.com/your-org/shelfy/compare/v1.0.0...v1.0.2
[1.0.0]: https://github.com/your-org/shelfy/releases/tag/v1.0.0
