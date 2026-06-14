# Installazione

Le release ufficiali sono su **[GitHub Releases](https://github.com/niccolofanton/shelfy/releases)**.
Scarica l'artefatto per il tuo sistema dall'ultima release.

> Le build **non sono firmate** con un certificato a pagamento (Apple Developer ID /
> Windows code-signing). L'app funziona ugualmente, ma il sistema operativo mostra un
> avviso al primo avvio: qui sotto come superarlo. Gli aggiornamenti successivi sono
> automatici in-app su **macOS e Windows**; su **Linux** per ora si aggiorna a mano (vedi sotto).

---

## macOS (Apple Silicon)

1. Scarica `SHELFY-<versione>-arm64.dmg`.
2. Apri il `.dmg` e trascina **SHELFY** in **Applicazioni**.
3. Al **primo avvio** macOS blocca l'app perché non notarizzata. A seconda della versione di
   macOS vedrai uno di questi avvisi:

   **Metodo A — "SHELFY Not Opened / Apple could not verify…" (macOS 15 Sequoia e successivi):**
   - Nell'avviso premi **Fine** (*Done*).
   - Apri **Impostazioni di Sistema → Privacy e Sicurezza**, scorri in fondo e clicca
     **Apri comunque** accanto alla notifica di SHELFY, poi autenticati con password / Touch ID.
   - Fallo **subito dopo** il tentativo di apertura: il pulsante compare solo per pochi minuti.
     Va fatto **solo la prima volta**.

   **Metodo A (bis) — click destro (macOS ≤ 14):**
   - In `Applicazioni`, **click destro** (o Ctrl+click) su **SHELFY** → **Apri**.
   - Nella finestra di avviso, clicca di nuovo **Apri**. Va fatto **solo la prima volta**.

   **Metodo B — Terminale (se compare "SHELFY è danneggiata e non può essere aperta", o se i
   metodi sopra non bastano):**
   Questo errore su Apple Silicon è dovuto all'attributo di *quarantena* di Gatekeeper su
   un'app ad-hoc non notarizzata. Si rimuove così:
   ```bash
   xattr -dr com.apple.quarantine /Applications/SHELFY.app
   ```
   Poi apri l'app normalmente.

> Perché succede: senza un certificato **Apple Developer ID** ($99/anno) l'app non può
> essere notarizzata, e macOS applica la quarantena. È un limite della distribuzione
> gratuita, non un problema dell'app.

## Windows

1. Scarica `SHELFY-Setup-<versione>.exe`.
2. All'avvio dell'installer, **SmartScreen** può mostrare *"Windows ha protetto il PC"*
   perché l'eseguibile non è firmato.
3. Clicca **Maggiori informazioni** → **Esegui comunque**. Solo la prima volta.

> L'aggiornamento Windows è un *self-rebuild*: serve **Node.js 22+** installato sul PC
> (l'app ricompila localmente l'installer). Dettagli in [windows.md](windows.md).

## Linux

1. Scarica `SHELFY-<versione>-x86_64.AppImage`.
2. Rendilo eseguibile e lancialo:
   ```bash
   chmod +x SHELFY-*.AppImage
   ./SHELFY-*.AppImage
   ```

> Nota: l'auto-update in-app per Linux non è ancora cablato nell'updater; per ora
> aggiorna scaricando la nuova AppImage dalle Release.

---

## Aggiornamenti

Una volta installata, SHELFY controlla gli aggiornamenti da sola e ti avvisa quando
una nuova versione è disponibile (impostazioni → canale stable/beta). Il feed è
servito da GitHub Releases, senza alcuna infrastruttura aggiuntiva.
