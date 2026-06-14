# Build e aggiornamenti su Windows

Guida unica per **buildare l'installer Windows** e capire come funziona
l'**auto-update self-rebuild**. Sostituisce le vecchie `GUIDA-WINDOWS.md` e
`WINDOWS-BUILD.md` (che si contraddicevano: vedi la nota in fondo).

La fonte di verità del comportamento è
[`build-windows.ps1`](../build-windows.ps1) e
[`electron/updater.js`](../electron/updater.js).

---

## In breve

- I moduli nativi (`better-sqlite3`) e i binari AI **non si cross-compilano dal
  Mac**: l'installer Windows va prodotto **su Windows** (oppure lo ricompila il
  client, vedi sotto).
- L'installer è **leggero**: i binari sidecar (yt-dlp, ffmpeg, llama, whisper)
  **non** sono dentro l'installer. Li scarica l'app stessa al primo
  avvio, **direttamente da upstream** (GitHub / gyan.dev), per variante GPU.
- L'aggiornamento Windows è **self-rebuild**: a ogni release si pubblica **solo
  il codice sorgente** sulla **GitHub Release**; il client rileva la nuova
  versione, scarica il source e **ricompila l'installer sul PC** con
  `build-windows.ps1`, poi lo esegue per aggiornarsi in-place. Serve **Node.js
  20+** sul client.

Vantaggi: niente installer Windows prebuilt da ospitare/firmare. Costo: sul PC
serve Node.js 20+ e la build dura qualche minuto. Il feed di rilascio lo produce
la CI a ogni tag (vedi §3), niente più infrastruttura Cloudflare R2.

---

## 1. Prerequisiti (una volta)

- **Node.js 20 LTS o più recente** → <https://nodejs.org> (serve sul PC che
  builda **e** su ogni client, perché l'update è un self-rebuild).
- Connessione a internet.
- Per **pubblicare** una release non serve nulla in locale: ci pensa GitHub
  Actions a ogni tag (vedi §3). Il build locale qui sotto serve solo a testare o
  a produrre un installer one-off.

> `build-windows.ps1` controlla la versione di Node e si ferma con un messaggio
> chiaro se è < 20 (sintassi JS moderna come `??=` fallisce su Node vecchi).

---

## 2. Buildare l'installer

1. Porta i sorgenti sul PC Windows (puoi escludere `node_modules/`, `dist/`,
   `release/`, `.vlm/`, `bin/`: si ricreano), oppure usa la *modalità autonoma*
   qui sotto.
2. Apri **PowerShell** nella cartella del progetto.
3. Prima volta nella sessione: `Set-ExecutionPolicy -Scope Process Bypass -Force`
4. Build:
   ```powershell
   .\build-windows.ps1                  # → release\SHELFY-Setup-<version>.exe
   .\build-windows.ps1 -Version 1.2.3   # build con versione esplicita (non tocca package.json)
   ```

Lo script: verifica Node ≥ 20 → `npm install` →
`electron-builder install-app-deps` (scarica un **prebuilt** di better-sqlite3
per l'ABI di Electron, **senza** Visual Studio / node-gyp) → `vite build` →
installer **NSIS** leggero in `release\`. Doppio click sull'`.exe` per
installare. Al primo avvio l'app scarica i componenti runtime per la tua
piattaforma.

### Modalità autonoma (senza copiare i sorgenti)

Scarica **solo** `build-windows.ps1` in una cartella vuota ed eseguilo: rileva
l'assenza dei sorgenti (`package.json` mancante) e li scarica dalla GitHub
Release (`SHELFY-src-latest.zip`), poi builda.

---

## 3. Rilasciare un aggiornamento (tag-driven via GitHub Actions)

Non serve buildare o pubblicare a mano: il rilascio è automatico a ogni tag.

```bash
npm version patch        # bump + tag vX.Y.Z (stable)
git push --follow-tags   # GitHub Actions builda e pubblica la Release
```

La CI ([`.github/workflows/release.yml`](../.github/workflows/release.yml)) genera
per Windows `SHELFY-src-<ver>.zip` + `source.json` (`{version, zip, sha512}`) e li
carica sulla GitHub Release. Una release **stable** nasce come *draft*: appena la
pubblichi, entro ~1 minuto ogni client Windows in esecuzione rileva la nuova
versione, scarica il source, **ricompila** l'installer e propone di riavviare.

Canale **beta**: `npm version prerelease --preid beta` → `source-beta.json`,
pubblicato su un tag rolling `beta`.

Dettagli del flusso in [`architecture.md` → Rilascio](architecture.md#rilascio) e
nel [README → Auto-update](../README.md#auto-update).

---

## 4. Come si comporta il client (self-rebuild)

1. Toast **"Aggiornamento disponibile"** → l'utente preme **Aggiorna ora**.
2. Scarica il source zip (sha512-verificato) → lo estrae in
   `%APPDATA%\shelfy\rebuild\src-<ver>\` → **compila** (mostra il log, teed in
   `rebuild\build-<ver>.log`).
3. Toast **"Aggiornamento pronto"** → **Riavvia ora** (rifiutabile; il riavvio
   resta disponibile in *Impostazioni → Aggiornamenti*). L'installer gira con
   `/S --updated --force-run`, sovrascrive in place e riavvia.

Stati interni: `available → downloading → building → built → installing`
(`error` con messaggio chiaro se manca Node ≥ 20). L'updater localizza Node
anche sotto version manager che la GUI non vede in PATH (`findBuildNodeDir()`:
fnm / nvm-windows / volta / scoop / Program Files); le directory sondate
finiscono in `rebuild\node-probe.log`.

Se manca **Node.js**, il self-rebuild si ferma: installa Node 22 LTS e riprova
(better-sqlite3 non pubblica prebuilt per Node più vecchi e tenterebbe di compilarli con Visual Studio).

---

## 5. Testare l'auto-update

1. Build+installa una versione bassa: `.\build-windows.ps1 -Version 1.0.2` →
   installa `release\SHELFY-Setup-1.0.2.exe`.
2. Pubblica una versione più alta con un tag (`npm version patch && git push
   --follow-tags`) e pubblica la Release draft prodotta dalla CI.
3. L'app 1.0.2 in esecuzione rileva la 1.0.3, scarica il source e si ricompila
   da sola.

> **Bootstrap:** un'installazione con un **vecchio** updater (pre self-rebuild)
> non sa ricompilarsi: va aggiornata una volta a mano installando un installer
> buildato con `build-windows.ps1`. Da lì in poi si auto-aggiorna.

---

## 6. Binari runtime — varianti GPU

I binari sidecar non sono nell'installer: l'app li scarica al primo avvio in
`…\AppData\Roaming\shelfy\runtime-bin\` (vedi `electron/binaries.js`). La
variante di `llama` si sceglie in *Impostazioni → Componenti runtime*:

- **NVIDIA (CUDA)** → `llama-…-cuda-12.4-x64` + runtime CUDA
- **AMD/Intel (Vulkan)** → `llama-…-vulkan-x64`
- **CPU** → `llama-…-cpu-x64`

Cambiando variante e premendo **Ripara**, riscarica solo `llama`. yt-dlp /
ffmpeg / whisper sono comuni a tutte le varianti. Il modello di visione (~3 GB)
**non** è incluso: si scarica al primo uso dell'analyzer.

### Sbloccare un'installazione vecchia che non trova i binari

Se l'analisi AI non parte / "Scarica" in Impostazioni va in errore su una
**vecchia** installazione, popola i binari da upstream senza ribuildare:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
irm https://raw.githubusercontent.com/niccolofanton/shelfy/main/provision-binaries.ps1 -OutFile provision-binaries.ps1
.\provision-binaries.ps1                 # NVIDIA/CUDA (default)
# .\provision-binaries.ps1 -Variant vulkan-x64   # AMD / Intel
# .\provision-binaries.ps1 -Variant cpu-x64      # solo CPU
```

> Dalle versioni recenti questo lo fa **l'app stessa** al primo avvio
> (Impostazioni → Componenti runtime → variante GPU → Scarica). Lo script serve
> solo a sbloccare installazioni vecchie.

---

## Note

- I sorgenti pubblicati: lo zip sulla GitHub Release è prodotto con `git archive
  HEAD` (`scripts/make-source-feed.ts`), quindi **solo file committati** — il
  working tree dirty non finisce nel pacchetto.
- macOS e Windows hanno feed separati: i client mac usano il `.dmg`
  (`latest-mac.yml`), i client Windows il self-rebuild (`source.json`).
- Se il download di `llama.cpp` fallisce (asset rinominato), apri la release
  <https://github.com/ggml-org/llama.cpp/releases/tag/b9500> e rilancia con la
  `-LlamaVariant` corretta.

### `-BinaryPack` è deprecato

`build-windows.ps1 -BinaryPack …` è **rimosso** (esce subito con un messaggio):
serviva a caricare su R2 i "binary pack". Oggi i client Windows scaricano i
binari **direttamente da upstream** per variante GPU, quindi non serve. I
mini-pack **macOS/Linux** (solo whisper + ffmpeg) li produce la CI con
`node scripts/make-binary-packs.ts` e li pubblica sulla GitHub Release.

---

## Nota sul consolidamento delle guide

Questo file unifica le due guide root precedenti, che divergevano:

- `WINDOWS-BUILD.md` descriveva l'update Windows come **NSIS auto-install
  silenzioso con blockmap differenziale** e presentava `-BinaryPack` come flusso
  attivo per i binari runtime su R2.
- `GUIDA-WINDOWS.md` descriveva il flusso **self-rebuild** corretto e segnava
  `-BinaryPack` come deprecato.

**Versione corretta = `GUIDA-WINDOWS.md`**, confermata da `build-windows.ps1`
(che builda NSIS leggero e tratta `-BinaryPack` come modalità separata/legacy) e
da `electron/updater.js` (che per Windows fa esplicitamente download del source
+ rebuild, **non** un OTA NSIS via electron-updater). La parte su NSIS
auto-install/blockmap di `WINDOWS-BUILD.md` era obsoleta. La sezione su canali
stabile/nightly di `WINDOWS-BUILD.md`, invece corretta, è stata assorbita qui e
in `architecture.md`.
