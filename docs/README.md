# Documentazione Shelfy

Indice della documentazione del progetto. Per la panoramica del prodotto parti
dal [README](../README.md) nella root.

> ⚖️ **Leggi prima le avvertenze legali.** Shelfy archivia contenuti che **tu** hai
> salvato nei **tuoi** account, per **solo uso personale**. L'accesso automatizzato può
> violare i ToS delle piattaforme e portare alla sospensione dell'account; sei l'unico
> responsabile dell'uso. Vedi **[../DISCLAIMER.md](../DISCLAIMER.md)** (EN/IT) — non è un
> parere legale.

## Guide principali

| Documento | Contenuto |
|-----------|-----------|
| [architecture.md](architecture.md) | Modello a 3 processi (main/preload/renderer), contratto IPC `window.electronAPI`, moduli `electron/` e `src/`, pipeline cattura→parse→DB→AI, auto-update, dati & sessioni. |
| [install.md](install.md) | Come installare su macOS/Windows/Linux (incl. sblocco Gatekeeper/SmartScreen) e come funzionano gli aggiornamenti. |
| [windows.md](windows.md) | Build dell'installer Windows e auto-update **self-rebuild**. |

## In root

| Documento | Contenuto |
|-----------|-----------|
| [../README.md](../README.md) | Panoramica, feature, getting started, auto-update, privacy. |
| [../DISCLAIMER.md](../DISCLAIMER.md) | **Avvertenze legali complete (EN/IT)**: uso previsto, ToS, copyright, privacy/GDPR, assenza di garanzie, limitazione di responsabilità, manleva. |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Prerequisiti, flusso di sviluppo, comandi di qualità, convenzioni commit, branch flow, dati. |
| [../CHANGELOG.md](../CHANGELOG.md) | Storico delle release (Keep a Changelog + SemVer). |
| [../LICENSE](../LICENSE) | Licenza **Apache-2.0**. |
| [../THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) | Attribuzioni e licenze dei componenti di terze parti (ffmpeg, yt-dlp, llama.cpp, whisper.cpp, Playwright, modelli AI). |

## Harness di valutazione AI

Strumenti di misura per il tuning della qualità AI (in `scripts/`):

- [search-eval](../scripts/search-eval/README.md) — retrieval della ricerca
  (TAGS / KEYWORDS / RESULTS).
- [extract-eval](../scripts/extract-eval/README.md) — estrazione tag/keyword vs
  oracolo curato a mano.
- [cluster-eval](../scripts/cluster-eval/README.md) — clustering dei tag (ARI,
  purity).

## Archivio

Documenti storici, in gran parte completati (vedi nota in testa a ciascuno):

- [archive/AI-TAGS-PLAN.md](archive/AI-TAGS-PLAN.md) — piano revisione tag AI &
  ricerca.
- [archive/CLEANUP-PLAN.md](archive/CLEANUP-PLAN.md) — piano di pulizia e refactor.
