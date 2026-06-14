# Changelog

Tutte le modifiche degne di nota a questo progetto sono documentate qui.

Il formato segue [Keep a Changelog](https://keepachangelog.com/it/1.1.0/) e il
progetto aderisce al [Versionamento Semantico](https://semver.org/lang/it/).

## [1.0.0] - 2026-06-14

Prima release pubblica open source.

### Aggiunto
- **Cattura passiva, senza credenziali** dei contenuti che hai salvato su
  Instagram, X e Pinterest: accedi in un browser integrato hardened e Shelfy
  legge le risposte delle API saved/bookmark delle piattaforme — nessuno
  scraping, nessuna API key, le credenziali non toccano l'app.
- **Website Analyzer**: cattura qualsiasi URL come reference di design —
  screenshot full-page in Chromium headless (Playwright), con estrazione
  deterministica di palette, tipografia, tech stack e award badge.
- **AI 100% on-device**: server `llama.cpp` con modelli vision Qwen3-VL / Gemma
  (GGUF) e output vincolato da JSON schema → descrizione, tag, entità, keyword,
  motivo del salvataggio e lingua per ogni elemento. Nessun cloud, nessun Python.
- **Libreria**: griglia masonry virtualizzata, ricerca full-text con rilevanza
  IDF, filtri faceted e chip AI, collezioni multi-membership, export JSON.
- **Ricerca conversazionale e vocale** (Chat) con `whisper.cpp` offline, e
  **Tags Explorer** con clustering dei tag (co-occorrenza Jaccard + embeddings
  locali `multilingual-e5-small`).
- **Download**: coda con `yt-dlp` + downloader HTTP, SSRF guard su ogni
  richiesta, ripresa automatica dopo il riavvio.
- **Motore di job in background** persistente (code che sopravvivono ai crash)
  con un Activity Center unificato (progresso, ETA, pausa/ripresa/annulla).
- **Auto-update** da GitHub Releases con artefatti verificati sha512 (macOS
  `.dmg`, Windows self-rebuild, Linux AppImage) e canali Stable / Beta.

### Note
- Multipiattaforma **macOS / Windows / Linux**. App non firmata (firma ad-hoc su
  macOS): vedi `docs/install.md` per lo sblocco al primo avvio.
- Rilasciata sotto licenza **Apache-2.0**. I componenti di terze parti (ffmpeg,
  yt-dlp, llama.cpp, whisper.cpp, Playwright e i pesi dei modelli) mantengono le
  proprie licenze — vedi `THIRD-PARTY-NOTICES.md`.
