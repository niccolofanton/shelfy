# Shelfy — inventario completo delle implementazioni al 3 ottobre 2026

## Perimetro e significato degli stati

- **Snapshot Git verificato:** `/Users/fant/work/experiments/shelfy-web-local/integration`, branch `web/foundations`, HEAD `7d91f2d`; include P2-14/F22 `30b4e97` e F20 `cc4f594`.
- **Perimetro:** tutte le **115 schede P1–P4**, più **F1–F22, UX-0–UX-10, X1–X7, P0 e P5** fuori conteggio. P6 storico è sostituito da X5 per E7/E10.
- **Fonti:** [EXECUTION](../EXECUTION.md), schede [P1](../phases/P1.md), [P2](../phases/P2.md), [P3](../phases/P3.md), [P4](../phases/P4.md), [piano](../IMPLEMENTATION-PLAN.md), [continuazione](continuation-2026-10-03.md), [passi live](live-steps.md), [audit UX](../reviews/ux-audit.md), Git e worktree. Le tabelle storiche sono riconciliate con l’evidenza nuova.
- **Integrata:** codice nel ramo comune, o azione operativa conclusa nel registro; P4-01 è assorbita da P2-04. Non implica distribuzione o accettazione finale.
- **Pending — in corso:** lane avviata/ripresa senza integrazione; l’avvio comunicato dal lead è un’assegnazione corrente, non monitoraggio continuo di un processo.
- **Pending — pronta:** risultato committato disponibile, ancora da ribasare/verificare/integrare o fermo su dipendenza concreta.
- **Pending — presa in carico, da avviare:** task assegnato alla continuazione Codex E20; nessuna chiusura pubblicata recuperata. Non significa implementazione pronta.
- **Da fare:** scheda ancora aperta senza lane corrente; eventuali passi/tooling parziali sono esplicitati.
- **Eliminata:** requisito ritirato dall’owner, solo P1-27.
- **Produzione registrata:** `server-v0.1.0-rc.5`, commit `58d3e2c`, deploy **3 ottobre 10:38 CEST**, osn `d28a1bc`. Il lead ha verificato via SSH durante la ripresa che il container usa ancora rc.5 e che Hermes è running senza restart; nessun nuovo deploy del drain o run completo di classificazione.
- **Ripresa E20/E21:** vecchie lane Claude riprese da Codex senza attesa reset crediti; dopo riavvio il lead ha riavviato dieci agenti Sol 6.1 high.
- **Priorità E22:** completare il miglioramento del harness AI, verificarlo e poi lanciare il tagging di **tutti i salvati Instagram del profilo owner**, ribadendo E15. X1 ed il deploy del drain sono prerequisiti operativi.
- **Metodo:** parser delle tabelle di fase con ID unici, verifica categorie disgiunte e conteggi; ogni hash Shelfy usato come prova di scheda integrata è verificato antenato del tip di integrazione indicato. Gli esiti test precedenti sono riportati dalle lane/registri, non rieseguiti qui; evidenze osn/owner storiche sono distinte dai controlli live aggiunti dal lead. Nel documento compaiono soltanto dati aggregati.

## Riepilogo delle 115 schede

| Fase | Integrate | Pending in corso | Pending pronte | Pending prese in carico, da avviare | Da fare | Eliminate | Totale |
|---|---:|---:|---:|---:|---:|---:|---:|
| P1 — Libreria web | 25 | 0 | 0 | 0 | 1 | 1 | 27 |
| P2 — Ingest e sync | 19 | 0 | 0 | 0 | 6 | 0 | 25 |
| P3 — AI | 23 | 1 | 0 | 0 | 7 | 0 | 31 |
| P4 — Capture, video e dati | 14 | 7 | 1 | 1 | 9 | 0 | 32 |
| **Totale** | **81** | **8** | **1** | **1** | **23** | **1** | **115** |

- **Pending complessive: 10.** F21 integrata resta fuori dal conteggio delle fasi.
- **Schede aperte: 33**, incluse 1 pronte da integrare. Il conteggio non misura lavoro pesato o accettazione live.
- **Correzione al vecchio tracking:** P2-14/F22 integrate; P2-12/P2-13 e UX-4/UX-6 già integrate dai commit cloud. F20 integrato e verificato nella CI 37144780896; Lighthouse resta F18.


- **Hardening job integrato c246490:** schema control 7, ID monotoni e checkpoint legati alla singola esecuzione logica; import legacy non associabili rifiutati con recupero tramite nuovo upload. Lane: 62/62 test post-rebase, 125 distinti complessivi e Clippy verdi; root: 82/82 test combinati queue/sync/tags/routes/sidebar. Nessuna verifica Rust complessiva sul nuovo candidato ancora dichiarata.

- **Verifica combinata lead su0b9edb3:** 1845/1845 test JavaScript in139file, tutte7configurazioni TypeScript e build web verdi dopo correzioni registri/eventi/i18n in f2cef46. Non copre gli ultimi backend STT/TTL né sostituisce il batch Rust. VPS letto alle22:26CEST: Docker29.6, rc.5 healthy, Hermes Up34h; nessun deploy nuovo. Runbook osn42e06ae corregge la precedente promessa di rollback N-1: schema7 richiede reader7, quindi niente rollback solo binario a rc.5.

- **Batch Rust lead su48c6d10:** 126 suite, 1747 test passati, 5 asserzioni fallite e 8 ignorati. Correzioni fixture/contratti in e35eed6 (tabelle control7, filtro lingua, accountId pairing e health capture); rerun mirato concluso: lib376/376, extension18/18, read_api18/18 e static_files9/9 verdi. Contratto TTL corretto7d91f2d e OpenAPI4/4; rootJavaScript1864/1864 in141file. Evidenza combinata batch+rerun, non nuovo batch completo dopo le correzioni.

## P1 — Libreria web: tutte le schede

| ID | Implementazione concreta | Stato | Evidenza e parte residua | Dipendenze |
|---|---|---|---|---|
| P1-01 | Realtime: bus SSE, notifiche, errori client e versione | Integrata | Commit integrato: `adc391a`. | T7, T11 |
| P1-02 | Shell responsive e manifest web | Integrata | Commit integrato: `960e246`. | T12 |
| P1-03 | Scritture libreria, cartelle, selettori, statistiche ed ETag | Integrata | Commit integrato: `b687c65`. Hardening F6 integrato; selector strict in P1-05. | T3, T11, P1-01 |
| P1-04 | Client comune: routing, SSE e gestione errori | Integrata | Commit integrato: `46fa823`. | T12, P1-01 |
| P1-05 | Ricerca, filtri, search-eval e admin synth/bench | Integrata | Commit integrato: `d5e5df9`. | T4, T11 |
| P1-06 | Modale post, cartelle e Sidebar sul client comune | Integrata | Commit integrato: `fbc575f`. 41/41 web e2e nel registro; cartelle e post sul client comune. | P1-03, P1-04 |
| P1-07 | Scheduler job e API job/code | Integrata | Commit integrato: `940c606`. | T7, P1-01 |
| P1-08 | Prestazioni Gallery e budget JavaScript | Integrata | Commit integrato: `e3144a2`. Budget storico 214 KB gzip; performance harness e windowing presenti. | P1-02, P1-05 |
| P1-09 | Server distribuibile: SPA, header, immagine e release | Integrata | Commit integrato: `c74cf63`. | T7, T12 |
| P1-10 | Merge nel core e parità golden | Integrata | Commit integrato: `ab6efbd`. | T3 |
| P1-11 | Cestino e operazioni massive per selettore | Integrata | Commit integrato: `7cb89a9`. F11 integrato; eliminazione cartelle oltre 500 post resta F17. | P1-03, P1-05, P1-07 |
| P1-12 | Backup, restore ed aggiornamento schemi | Integrata | Commit integrato: `9b8183c`. Review backup/restore corretta in F3/F4. | T3, T7 |
| P1-13 | Passkey, riautenticazione, login iniziale e SMTP | Integrata | Commit integrato: `dfd10e9`. | T10 |
| P1-14 | Selezione Gallery, bulk, facet e Cestino | Integrata | Commit integrato: `c58da0f`. Select-all-matching, bulk con undo, facet e round trip real server. | P1-05, P1-06, P1-11 |
| P1-15 | Metriche, redazione log e rate limit | Integrata | Commit integrato: `42d96e6`. | P1-07, P1-09 |
| P1-16 | Prima PR osn: preparazione infrastruttura | Integrata | Commit integrato: `c7a47e0`. | P1-09, P1-12 |
| P1-17 | API account, token e device-code flow | Integrata | Commit integrato: `c95378c`. Include F5; gli scope di accesso alla libreria restano F21. | P1-07, P1-13 |
| P1-18 | Prima PR osn: apply servizi, fase 1 | Integrata | osn PR #29 merged, osn fa2066e, rc.1 healthy; evidenza live storica con Hermes verificato. | P1-15, P1-16 |
| P1-19 | Migrazione desktop ed installazione tramite job | Integrata | Commit integrato: `517c139`. | T9, P1-07, P1-10, P1-17 |
| P1-20 | Accesso, riautenticazione, approvazione device ed Impostazioni | Integrata | Commit integrato: `9292986`. | P1-04, P1-13, P1-17 |
| P1-21 | Web e2e e Lighthouse in CI | Integrata | Commit integrato: `a563e90`. Gate mock e real server; Lighthouse è non bloccante, LCP resta F18. | P1-08, P1-09, P1-14, P1-20 |
| P1-22 | Bucket R2 e token scoped per backup | Integrata | O2 conclusa: bucket R2 e token scoped; osn 1ce0abe, r2-backup-check registrato. | — |
| P1-23 | Prima PR osn: DNS, Access e backup, fase 2 | Integrata | osn 44926e5/e65a777, rc.2, backup DB/media e restore di due DB; misure SSE spostate a P1-26. | P1-18, P1-22 |
| P1-24 | SPIKE-8: passkey sui dispositivi del proprietario | Integrata | Commit integrato: `42a22f7`. Conferma owner su tutti i dispositivi; primo accesso passkey documentato. | P1-02, P1-13, P1-18, P1-20 |
| P1-25 | Libreria reale sul VPS e restore drill | Integrata | Commit integrato: `d54464a`. Report p1-vps: 6.138 post, 980,9 MiB, conteggi coincidenti e restore green; controllo visivo owner separato. | P1-19, P1-23 |
| P1-26 | Budget prestazionali, SSE e restart sul VPS | Da fare | Tooling pronto 75ee458 in scripts/live; mancano misure definitive dopo deploy F8, SSE lungo/resume e restart. | P1-05, P1-15, P1-25 |
| P1-27 | Uso quotidiano owner come uscita P1 | Eliminata | Ritirata dal proprietario per E13 (42a22f7). | P1-23–P1-26 |

## P2 — Ingest e sincronizzazione: tutte le schede

| ID | Implementazione concreta | Stato | Evidenza e parte residua | Dipendenze |
|---|---|---|---|---|
| P2-01 | SPIKE-9: video on demand ed idratazione link | Integrata | Commit integrato: `188dc1d`. Report SPIKE-9 e decisione L17; probe video ed idratazione concluse. | — |
| P2-02 | Sanitizzazione ingest e stato archivio nel core | Integrata | Commit integrato: `3b11ad9`. 13 batch golden; una regola archive-state per ingest/drain/migrazione. | — |
| P2-03 | Pairing, config estensione, kill switch e presenza | Integrata | Commit integrato: `dbdd1e3`. Control v4, pairing 60 s, gate versione 426, flag e presenza. | — |
| P2-04 | HTTP outbound, CDN, limiti host e breaker | Integrata | Commit integrato: `8463ffb`. Assorbe P4-01 per L11; outbound unico e host breaker. | — |
| P2-05 | Parser: URL video diretti ed ingresso Instagram REST | Integrata | Commit integrato: `a70f7dd`. Desktop e2e 76/76 nel registro. | — |
| P2-06 | Core estensione: build, pairing, API, offline e cattura passiva | Integrata | Commit integrato: `811410e`. 207 unit e 29/29 smoke nel registro; account reali in P2-23. | — |
| P2-07 | PWA, share Android, pagina /share e bookmarklet | Integrata | Commit integrato: `20d21c7`. | — |
| P2-08 | Centro notifiche web con notifiche e job | Integrata | 7048243; centro notifiche persistenti, paging/SSE, controlli job. Verifica combinata lead: 172 test pass inclusi attività/i18n/browser. | P4-09 (L14), P1-01; JobsApi/useJobs |
| P2-09 | API ingest, sync run e mapping cartelle/board | Integrata | Commit integrato: `eb25593`. Library v4, sync.progress; p95 ingest 280 ms nel registro. | P2-02, P2-03 |
| P2-10 | Drain archivio di cover e media | Integrata | Commit integrato: `0c2d532`. Cover prioritarie, CAS, rendition, quote e fallback; già nella rc.5. | P2-02, P2-04; P4-07 (L13) |
| P2-11 | API link ed idratazione social server | Integrata | Commit integrato: `c7590d3`. Placeholder immediato e link.hydrate IG/X/Pinterest; già nella rc.5. | P2-02, P2-04; P2-01 come input |
| P2-12 | Connessioni: pairing, stato estensione e Shortcut | Integrata | Commit integrato: `ffa06ba`. Bridge pairing e Shortcut verificati con fakes; verifica vera ancora aperta. | P2-03, P2-06 |
| P2-13 | Controller sync estensione: replay, scroll e resume | Integrata | Commit integrato: `262bf7e`, `139c480`, `512f96c`, `227f9cf`. Smoke 25/25 su pagine sintetiche; prime sync vere in P2-23. | P2-06 |
| P2-14 | API task estensione, lease e upload media | Integrata | Commit integrato: `3b538de`, `4a65aee`, `30b4e97`. F22 integrato: completion valida eligibility corrente e lease holder; non più pending. | P2-03, P2-10, P2-11; P4-08 (L12/L23), F22 |
| P2-15 | Sync sorgenti, promemoria e avvio dal web | Integrata | 5246146; planner e promemoria integrati. Vincolo account corretto in P2-17; review indipendente conclusa prima del deploy. | P2-09, P2-13 |
| P2-16 | Overlay selezione post nell’estensione | Integrata | 1c98ee8 + 2d2cdc8; overlay e batch vincolati al pairing; review indipendente e verifica combinata 448 test + typecheck verdi. | P2-06, P2-09 |
| P2-17 | Worker estensione: upload, refresh ed idratazione Instagram | Integrata | 3e44bef; refresh/upload poster, lease e vincolo account anche per queue legacy. Review indipendente chiusa; 313 test estensione e smoke sintetici verdi nella lane. Sync live resta aperta. | P2-05, P2-13, P2-14, P2-15 |
| P2-18 | Controlli sync web ed attività di sincronizzazione | Integrata | 5de43ac; review indipendente approva binding account C9 e visibilità errori pre-C4. Root 69 test sync/sidebar/routes verdi. DTO C2 accountId richiede rigenerazione OpenAPI nel batch Rust. | P2-08, P2-09, P2-12, P2-15, P1-06, P1-14 |
| P2-19 | E2E estensione in CI e strumenti parità | Integrata | Commit6d87124 (9ab8df0 ribasato): 323 test estensione, E2E server reale 11 checkpoint/51s/0retry. Review indipendente senza finding,81test rieseguiti; root suiteJS1864/1864. CI cloud e sync owner restano separate. | P2-09–P2-17, P1-21 |
| P2-20 | Deploy P2 e configurazione VPS | Da fare | Parziale storico: rc.5 con archive/link; restano deploy P2 completi, config estensione/Access e prove live. | rc.1: P2-03, P2-09, P2-10 |
| P2-21 | Completamento SPIKE-3 sugli account reali | Da fare | Parziale: IG folder 123/123 e primo campione X; restano X completo con baseline desktop e Pinterest se usato. | — |
| P2-22 | Probe Access con estensione reale | Da fare | Probe client reale via Access ancora aperto. | P2-06 |
| P2-23 | Prime sync e prove dispositivi sul live | Da fare | Mancano sync reali IG/X/Pinterest e prove dispositivi; synthetic smoke non chiude la scheda. | P2-07, P2-11–P2-13, P2-15, P2-20 rc.2, P2-22 |
| P2-24 | Parità estensione contro desktop | Da fare | Parità completa estensione/desktop da misurare dopo le prime sync reali. | P2-19, P2-23 |
| P2-25 | Sette giorni di sync quotidiane come uscita P2 | Da fare | Sette giorni di sync ancora richiesti: E13 elimina soltanto P1-27. | P2-17, P2-18, P2-23 |

## P3 — AI: tutte le schede

| ID | Implementazione concreta | Stato | Evidenza e parte residua | Dipendenze |
|---|---|---|---|---|
| P3-01 | Adapter AI OpenAI-compatible, Anthropic, Whisper e stub | Integrata | Commit integrato: `90dfa24`, `baaed4e`. Adapter/stub, 135+ test e probe nodo nel registro. | — |
| P3-02 | Vault chiavi e master key | Integrata | f03c457; vault e rotazione integrati dopo review; ai_service11/key_vault5 verdi. | P1-12 |
| P3-03 | Prompt/schemi AI v2 condivisi e core catalogo | Integrata | Commit integrato: `8a803be`, `5fc8c93`, `151ba35`. 22 richieste desktop e 154 casi golden nel registro; scorer condiviso. | P1-03, P1-10 |
| P3-04 | Dati tag: explorer, salute, merge, rename e facet | Integrata | 3a03d2c; explorer/tag/health/merge/facet e rename atomico, golden parità. Benchmark 20k: p95 GET a cache calda 0,083–0,745 ms; primo calcolo merge-suggestions 2,157 s. Suite combinata in corso. | P1-03, P1-05; sequenza dopo P3-06 |
| P3-05 | Ranking tag nella ricerca | Integrata | 27262f6 + e7a715a; ranking IDF e alias coerenti anche nei selettori della coda AI. Release20k tag-only p95 0,61 ms. | P1-05 |
| P3-06 | Core cluster/alias e API di revisione | Integrata | 0aa28f9; core cluster/alias e API di revisione integrati, golden/test/clippy verdi. | P1-03 |
| P3-07 | Retrieval chat e pool search-eval | Integrata | 2548281; retrieval indicizzato, cache isolata per utente, parser e pool parità. Sul tip combinato 6 retrieval + 32 golden + 11 search-eval passano; 2 generatori fixture ignorati intenzionalmente. | P1-05 |
| P3-08 | Client AI comune desktop | Integrata | Commit integrato: `27985d1`. Client AI comune presente; sul web capacità disattivate finché le API non arrivano. | P1-04 |
| P3-09 | Servizio AI e provider operatore | Integrata | Commit integrato: `25de428`. Routing vision/testo, pacing, breaker, health, usage, consenso ed eventi provider. | P3-01 |
| P3-10 | SPIKE-6 completo sul nodo owner | Da fare | Probe preliminari esistono; SPIKE-6 e gate completo non conclusi. | P3-01, P3-03 |
| P3-11 | Esploratore tag web | Integrata | 9fc5377; explorer web, tier, filtri, cartella completa, merge/alias persistenti. Lane: 1742 unit, 76 desktop E2E e test real-server dopo rebase verdi; job controls in P3-23. | P3-04, P3-06, P3-08, P1-06 |
| P3-12 | Bridge eval e cluster-eval | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P3-05, P3-06, P3-07 |
| P3-13 | Catalogazione social: ai.drain, analyze e coda | Integrata | 7b4bd8b; drain persistente, API analyze/queue, admin e guardie anti-stale/manual edit. Review indipendente chiusa; deploy/run owner ancora da fare, CPU budget non attestato. | P3-03, P3-09 |
| P3-14 | Ricerca conversazionale tramite SSE | Integrata | 8157df2; SSE conversazionale, scope, cancel e BYOK. Lane: 38 test server e 56 core verdi; verifica combinata del nuovo candidato ancora da eseguire. | P3-03, P3-07, P3-09 |
| P3-15 | API suggerimenti a chip | Integrata | Sorgente7d360b4 con fix2ff0dd5/a9979ac; root9/9test search_suggest passati nel batch48c6d10. Auth/consenso/cache/errori/rate verificati; OpenAPI/client rigenerati. | P3-03, P3-07, P3-09 |
| P3-16 | Job clustering ed alias | Integrata | 78446c9; job cluster/alias persistenti, ripresa offline, cancellazione e cache embedding; review lead completata, 20 test combinati run/queue/OpenAPI verdi. | P3-03, P3-06, P3-09 |
| P3-17 | Dettatura e trascrizione | Integrata | Backend509b5c0 e frontend4819108: WAV120s, lingua, rate limit, route STT e cancel. Lane42 test backend+Clippy e63 frontend verdi; server reale chat/dettatura sul candidato ancora da eseguire. | P3-08, P3-09 |
| P3-18 | Impostazioni AI: provider, routing, utilizzo e stato | Integrata | b940770 + c2147ca; sei impostazioni AI allowlisted, stato provider e routing UI; 288 regressioni AI e smoke real-server verdi nella lane, 27 test account/service/OpenAPI verdi sul tip combinato. | P3-08, P3-09 |
| P3-19 | BYOK: API, chiavi, test e consenso | Integrata | 6157709; BYOK sigillato, test sintetici, consenso e cancellazione al cambio configurazione. Review lead e 35 test combinati provider/redazione/servizio/trasporto/OpenAPI verdi. | P3-02, P3-09 |
| P3-20 | UI coda AI, pannello post ed analisi selezione | Integrata | c365289; coda persistente, preventivo/conferma, pausa/riprendi/cancella, pannello post e collegamento provider. Lane 388 test e 3 browser real-server verdi; stime mixed web in P3-27. | P3-13, P3-18, P1-06, P1-14 |
| P3-21 | Gallery AI: suggerimenti e facet dinamici | Integrata | Sorgente150b82a: chip opt-in, facet e lingua. Lane433testJS/3E2E; rootread_api18/18 dopo fixture48c6d10/e35eed6, core lingua/none nel batch; rootJS1864/1864. Accettazione live separata. | P3-04, P3-08, P3-15, P1-14 |
| P3-22 | Interfaccia AI Search web | Integrata | 4819108; chat SSE, filtri/provider, mobile e frontend dettatura. Lane 85 test mirati + 3 Playwright e typecheck verdi; backend P3-17 ora integrato; gate real-server nuovo candidato ancora aperto. | P3-05, P3-14, P3-17, P1-06 |
| P3-23 | Avvio cluster ed alias dalla UI | Integrata | b643fc6; cluster/alias con SSE, polling e cancel ID esatto. Lane 345 test + 7 typecheck; real-server sintetico verde con binario precedente allo schema7, da ripetere sul candidato. | P3-11, P3-16 |
| P3-24 | Extract-eval sulla pipeline web | Pending — in corso | Checkpoint0b9edb3: actual worker/CAS/stub, due profili20 e gate missing/unreadable poster. Lane4Rust+4TS passano. Adapter reale20 e confronto desktop ancora aperti; scorestub1.000 non è qualità modello. | P3-13 |
| P3-25 | Operazioni AI: dashboard, alert, stack test e runbook | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P3-09, P3-13 |
| P3-26 | Wizard UI di collegamento provider BYOK | Integrata | 1815524; wizard BYOK e descrittori per-task integrati. Root 15 test UI/provider verdi; lane Playwright sintetico e static checks green. Verifica Rust combinata successiva. | P3-18, P3-19 |
| P3-27 | Catalogazione siti e controllo qualità screenshot | Integrata | 5bd2c72; catalogo design con 19 facet/evidence, QC e preventivi web/social. 54 core + 16 server, Clippy e golden verdi. Media 4/768 conforme scheda; overview/budget desktop e CPU40ms/post non attestati, provider live ancora aperto. | P3-13, P4-14 ingest capture |
| P3-28 | Deploy AI sul VPS, nodo e catalogazione almeno 5.000 post | Da fare | Deploy drain/setup provider e classificazione live non avviati; servono F20, backup, gold valido ed Hermes verificato. | P3-13, P3-18, P3-20, P3-25 |
| P3-29 | Gate qualitativi reali AI | Da fare | Gate reali da concludere; self-check gold non è qualità del nodo. | P3-10, P3-12, P3-16, P3-24, P3-28 |
| P3-30 | Ritaratura pesi FTS sui campi AI | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P3-05, P3-07, P3-12, P3-28 |
| P3-31 | Accettazione finale owner P3 | Da fare | Accettazione owner da ottenere; P3-27 può seguire P4. | tutte le altre P3; P3-27 può seguire P4 |

## P4 — Catture, video e dati: tutte le schede

| ID | Implementazione concreta | Stato | Evidenza e parte residua | Dipendenze |
|---|---|---|---|---|
| P4-01 | Client egress unico per richieste outbound | Integrata | Commit integrato: `8463ffb`. Assorbita da P2-04 per L11; contata una scheda soddisfatta, senza duplicare codice egress. | — |
| P4-02 | Proxy egress, policy e suite SSRF | Integrata | Commit integrato: `65f4ce0`, `81feadb`. 92 probe SSRF nel registro; CI resa affidabile in F20 cc4f594. | P4-01 (dipendenza tardiva) |
| P4-03 | Servizio Node capture v2 | Integrata | Commit integrato: `bbb6410`, `ad94b4c`. Node, Chromium proxy e NDJSON; 26 test più cattura reale nel registro. | — |
| P4-04 | Core catture web, versioni ed eliminazione | Integrata | Commit integrato: `53755a7`. 17 test core nel registro. | — |
| P4-05 | Core elenco siti, facet, colore e similar | Integrata | Commit integrato: `7aaccba`, `e435257`. Listing, OKLab, facet e similar su quattro set golden. | — |
| P4-06 | Strumenti yt-dlp e ffmpeg | Integrata | Commit integrato: `e47260a`, `5746edd`. Video tools, cancellazione processi, poster/keyframe; determinismo CI F20 integrato. | — |
| P4-07 | Quote, contabilità utilizzo e limiti | Integrata | Commit integrato: `6f1d758`, `c1fc11f`. Reservation, accounting/media budget; 50 concorrenti nel registro. | — |
| P4-08 | Upload tus web con sessioni, scope e purpose | Integrata | Commit integrato: `46c6ba1`, `4a93d13`. Sessioni/scopi tus, purpose registry ed upload monouso. | — |
| P4-09 | Vista Attività/Jobs e controlli code | Integrata | Commit integrato: `67d1c5c`. Paging/SSE/queue controls; rifinita da UX-6 e F19. | — |
| P4-10 | Import v1: desktop, array ed export estensione | Integrata | 2406d9d; parser JSON streaming, import metadata con claim atomico e checkpoint/report. Lead: 18 test import/export/OpenAPI verdi; lane: 135 test, parser >200 MiB con picco RSS 7,28 MiB. | P4-08 (dipendenza tardiva) |
| P4-11 | Export v2 di libreria ed oggetti | Integrata | 44b7518; export ZIP64 lossless con recovery/TTL/range, review indipendente conclusa. CONTROL0005 exports e CONTROL0006 token libreria integrati insieme; test mirati combinati verdi. | — |
| P4-12 | Garbage collection oggetti e risorse | Integrata | 99b0743; GC e retention con interlock export. Exact lane GC/export 18/18 verdi. Fix riuso ID job separato, in corso prima del deploy. | — |
| P4-13 | Immagine capture, seccomp ed isolamento | Integrata | ffaa02d; immagine capture e guard sui listener integrati. Verifiche lane green; full compose Docker >=28 resta gate della nuova CI. | P4-02, P4-03 |
| P4-14 | Job capture ed ingest artefatti | Integrata | f524017 + e81bc5c + 75d7d80; ingest/cancel/quote/receipt e hook AI. Review lead corregge deadline L21 a13min sopra service12. Lane12 capture verdi; root29 test JS combinati verdi; Rust complessivo/schema generato e VPS ancora da verificare. | P4-03, P4-04, P4-07 |
| P4-15 | API siti web | Pending — pronta | Candidati 3156b72 + e4aac21: API siti/versioni/facet/similar, synth e ripristino ThumbHash. Solo controlli statici: compilazione, OpenAPI e p95 pendenti. | P4-04, P4-05; sequenza dopo P4-14 |
| P4-16 | Video on demand: resolver, cache e conserva offline | Pending — in corso | Lane Codex avviata su resolver video, cache e conserva offline; quote/egress/SSRF e cancellazione da verificare. Nessun download della libreria owner avviato. | P4-01, P4-06, P4-07, P1-11 |
| P4-17 | Rimozione copie archiviate e video conservati | Da fare | Quote liberate al GC; sequenza lead dopo P4-16. | P4-07, P1-11; sequenza dopo P4-16 |
| P4-18 | API segnalibri manuali | Pending — in corso | Lane Codex ripresa dopo integrazione correzione ID job c246490; implementazione completa e verifiche della scheda ancora aperte. | P4-07, P4-08 |
| P4-19 | Import v2 e round trip senza perdite | Pending — in corso | Lane Codex attiva su import v2 e round trip completo; base48c6d10, schema7 e checkpoint fail-closed. Verifiche ancora aperte. | P4-10, P4-11 |
| P4-20 | Reset account/libreria nella zona pericolosa | Pending — in corso | Lane Codex avviata sui reset account secondo scheda completa. Nessuna azione owner/live; implementazione e verifiche ancora aperte. | P4-12, P1-11 |
| P4-21 | Impostazioni Dati: import ed export | Pending — in corso | Lane Codex avviata su Impostazioni Dati, import/export e integrazione contratti P4-19/P4-18. Verifiche ancora aperte. | P4-10, P4-11 |
| P4-22 | Feedback tramite relay server | Pending — presa in carico, da avviare | Presa in carico E20; nessun risultato pubblicato recuperato.  Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-01 |
| P4-23 | Interfaccia Siti web | Pending — in corso | Lane Codex attiva su Siti web: adapter desktop, elenco, dettagli e versioni. Prime verifiche TS e 71 test verdi; gate server/fake capture ancora aperto. | P4-14, P4-15, P1-06 |
| P4-24 | UI video: riproduci, conserva, rimuovi ed elenco offline | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-16, P4-17, P1-06, P1-14 |
| P4-25 | Interfaccia segnalibri manuali | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-18, P4-21 (dipendenza tardiva), P1-06 |
| P4-26 | Impostazioni Zona pericolosa | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-20 |
| P4-27 | E2E capture in CI e baseline desktop | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-02, P4-13, P4-14 |
| P4-28 | Seconda PR osn: preparazione capture/egress | Pending — in corso | Lane Codex attiva su preparazione OSN capture/egress, base osn42e06ae, worktree isolato. Nessun apply o riavvio produzione. | P4-02, P4-13, P4-14 |
| P4-29 | Condivisione file Android | Da fare | Nessuna chiusura integrata recuperata; completare implementazione e verifiche della scheda. | P4-25, P2-07 PWA/share target |
| P4-30 | Seconda PR osn: deploy capture/egress/video | Da fare | Apply delle tre immagini, isolamento e rollback ancora da eseguire con baseline Hermes. | P4-28, P4-16, P4-22 |
| P4-31 | Misure VPS catture, video ed isolamento | Da fare | Mancano misure corpus capture/video/isolation su VPS finale. | P4-30, P4-27 |
| P4-32 | Accettazione finale owner live P4 | Da fare | Controllo finale owner su dati, siti, video, bookmark e reset live ancora aperto. | P4-21, P4-23–P4-26, P4-30 |

## Follow-up F1–F22: fuori dalle 115 schede

| ID | Correzione concreta | Stato | Evidenza, dipendenza e residuo |
|---|---|---|---|
| F1 | Hardening login: redeem POST/fragment, CSRF, proxy fidati, sessioni revocate e auth deny-by-default | Integrata | `b8503e0`, `178d675`, `0d66c7b`; chiude review T10 e decisioni L1–L3. |
| F2 | Riallineamento dei sette desktop e2e preesistenti | Integrata | `b6bdcb3`, `5973b84`; 76/76 desktop nel registro; spec download raggruppati e URL X aggiornate. |
| F3 | Non consumare retry dei job mentre una libreria è bloccata per restore | Integrata, assorbita da F4 | `b0a73a7`: requeue senza tentativo per user_locked. |
| F4 | Hardening backup/restore e migrazioni: handle, segnali, lock, snapshot incompleti e review finding | Integrata | `6c68126`, `dad66a8`, `587484e`, `95bc5a8`; race e restore corretti; rollback writes sono limite documentato. |
| F5 | Evitare riuso ID passkey e ambiguità audit | Integrata in P1-17 | `47bbab0`: CONTROL v3, rebuild passkeys con AUTOINCREMENT. |
| F6 | ETag/statistiche, announce dopo commit, cache eviction, PATCH identiche, manual AI e limiti byte | Integrata | `c970498`, `cb5b938`, `484ad9d`, `f823b12`; include merge idempotente con chiave ripetuta. |
| F7 | Race di persistenza useDownloadPrefs | Integrata | `49b22c7` insieme a F2: updater puro e scrittura in effect. |
| F8 | Histogram media con label variant per isolare budget g480 | Integrata | `0cb019d`; budget live necessita deploy della metrica e prime richieste media, P1-26. |
| F9 | TTL server-side dei token API ed opzione scadenza | Integrata | Sorgente3ef5b10: default90giorni,1–365, UI7/30/90/365. RootTTL2/2, account/library_tokens nel batch, schemaoptional corretto7d91f2d con OpenAPI4/4; rootJS1864/1864. Legacy e device7giorni preservati. |
| F10 | Evitare 429 prematuri nell’approvazione device che richiede re-auth | Integrata | `27a4f7b`, `170b30e`; refund reauth_required e Approve disattivato finché non c’è prova recente. |
| F11 | Purge trash, checkpoint bulk, undo job queued, idempotency 423 e retention | Integrata, con F17 separata | `2aa169d`, `57030b6`, `61028d2`, `efea570`, `402c9d6`, `8b267ed`, `36e97a0`; L3 trasferita a F17. |
| F12 | Preservare palette/font/tech/awards nella migrazione di capture web | Integrata e verificata live | 814a031; backfill idempotente applicato ai due profili live dopo backup. API autenticata conferma palette/font/tech/awards in owner e mock, sessioni di verifica revocate, Hermes invariato. |
| F13 | Test timing sensibili a forte carico macchina | Integrata | aaa1efa; timing deterministici, CI release20k e Vitest forks2. 1498 test pass nella lane; 172 test combinati sul ramo lead pass. |
| F14 | Recuperare margine del bundle caricando i18n per vista | Integrata | `5305c97`; 218,3 → 177,1 KB gzip nel registro; nuove UI devono mantenere lazy namespace e gate. |
| F15 | Timeout connect AI per purpose e corretta classificazione nodo offline | Integrata | `e59998f`; operator connect 3 s, errore Connect/Offline, loopback solo test. Drain deve gestire pausa QuotaExhausted. |
| F16 | Preservare ordine originale dei campi dello schema nelle richieste AI | Integrata | `d61e89e`; RawValue nei body OpenAI/Anthropic; P3-13 usa JsonOutput::from_raw. |
| F17 | Eliminare cartelle oltre 500 post tramite job chunked con undo/idempotency | Pending — pronta | Candidato 86e82b7: job chunked oltre500, dedupe, undo e replay. 37 test TS verdi; nove regressioni Rust e schema pendenti. |
| F18 | Ridurre LCP Gallery sotto il budget sul cold load slow-4G | Pending — in corso | Audit Chrome DevTools attivo; cold/warm cache da distinguere. Budget Lighthouse 2,5 s ancora non chiuso. |
| F19 | Refresh riepilogo coda dopo cancel/retry di un singolo job | Integrata | `d305f7d`; test real-server non più fixme e unit useJobs; verifica della release ancora da eseguire. |
| F20 | Rendere deterministici readiness SSRF ed ordinamento cancellazione video in CI | Integrata | `cc4f594`; review lead e verifiche locali: **92/92 SSRF** (89 rifiuti + 3 positivi), **38/38 video**, **3/3 cold-start IPv6**, clippy media verde; nuova CI remota non ancora verificata. SSRF resta gate bloccante. |
| F21 | Token library:read/library:write con registry authz least privilege | Integrata | 44b7518; library:read/library:write e registry authz integrati dopo export, CONTROL0006 e fixture v6. Review indipendente completata; token e schema verificati sul tip combinato. |
| F22 | Rifiutare completion upload forgiate/stale e da holder di lease diverso | Integrata con P2-14 | `30b4e97`: eligibility corrente atomica, binding holder/generazione ed aggiornamento API/test; chiude blocker della review `c16c863`. |

**Conteggio follow-up:** 19 integrati, due presi in carico da avviare, uno in corso = 22. F12 ha completato anche backfill e verifica API live.

## UX-0–UX-10: rifinitura e verifiche, fuori dalle 115 schede

| ID | Implementazione concreta | Stato | Evidenza e dipendenze |
|---|---|---|---|
| UX-0 | Harness visuale/accessibilità real server, quattro viewport, overflow, hit area, stacking, focus, contrasto e parità i18n | Integrata | 3936c54; harness real-server, 88/88 condivisi e78/78 standalone nella lane, 159 i18n pass sul ramo lead. Difetti KNOWN ancora espliciti e controllati. |
| UX-1 | Shell mobile, MenuButton in flow, drawer dialog, safe area, dvh, Search focus ed offline pill | Integrata | `cab7eef`, `ed35020`; 29 nuovi Playwright su quattro viewport; prova iOS/Android reale ancora richiesta. |
| UX-2 | Design token, contrasto/focus/layering, Button/IconButton/EmptyState/Notice/Spinner/PageHeader/toast/dialog/popover sheet | Integrata | `c2f8673`, `1d71eeb`, `3e59d60`, `a002e2b`, `67cefa3`; primitives condivise per tutte le lane UX successive. |
| UX-3 | Gallery mobile: toolbar 390 px, filter sheet/overlay/panel, selection bar ed empty state | Integrata | `93e9b74`, `2a0a194`; 101/101 web Playwright nel registro, combinazione con UX-5 verificata. |
| UX-4 | Card: fallback, ring selezione, fade testo, checkbox touch 44 px ed alt | Integrata | `76ef930`; test post-card presenti; screenshot dell’harness completo aspettano UX-0. |
| UX-5 | Modale full-screen sopra la shell, lightbox, safe area, fallback media, note/tag personali separati dall’AI e swipe | Integrata | `e04aa22`; 108/108 web Playwright nel registro. |
| UX-6 | Cestino e Jobs: primitive, conferme, toast, queue menu, layout e label | Integrata | `035fe2b`; test trash-jobs-mobile; italiano Jobs ora Attività, da distinguere dal centro notifiche P2-08. |
| UX-7 | Impostazioni mobile, account/storage/provider, lingua e copy | Da fare | UX-1/UX-2 pronte; sequenza lead dopo P2-12 e P3-18, per evitare rifinitura di UI non ancora completata. |
| UX-8 | Login/re-auth/share/device: priorità azioni, input iOS e gestione errore | Da fare | UX-2 e contratti auth/share; preservare hardening F10. |
| UX-9 | Dialoghi/cartelle: label, focus trap, conferme e movimento ridotto | Da fare | UX-2; sequenza dopo P4-22 per includere feedback completo. |
| UX-10 | Adozione token e reduced motion nelle viste residue desktop/AI/siti/browser | Da fare | Tutte le altre UX e viste funzionali integrate; sweep finale e desktop e2e. |

**Conteggio UX:** sette integrate, quattro da fare. L’audit X4a è concluso; il completamento degli interventi X4 resta aperto.

## X1–X7: aggiunte del proprietario, fuori dalle 115 schede

| ID | Risultato richiesto | Stato | Evidenza, dipendenze e residuo |
|---|---|---|---|
| X1 | Gold benchmark AI, miglioramento harness e classificazione completa | Pending — in corso | Harness e prompt integrati 902c44b. Confronto controllato 8 casi: composite .416→.454, entity F1 .556→.700. Full40: 38 validi, 2 gated, zero errori; composite .437 totale/.460 completi. Report x1-node-benchmark-2026-10-03.md. Classificazione owner e deploy ancora da eseguire. |
| X2 | Account mock live con circa 500 post e strumenti di creazione/migrazione | Integrata e operativa nel registro | `cab1d6d`, `3b3bff9`, `a4ad01b`, `5666df4`; 500 post, 554 oggetti, 125,4 MiB e conteggi reconciliati; base dei test finali. |
| X3 | Test di ogni funzione web sul mock, suite e pass interattivo in tutte le sezioni | Da fare | Dopo P2–P4 ed X1 run; scroll, tap, avvio di tutte le azioni e matrice rispetto a features/. Le suite parziali presenti non equivalgono all’accettazione finale. |
| X4 | Audit design e migliorie web/mobile/desktop senza redesign | Pending — in corso complessivamente | Audit X4a `db7ef1a` concluso; UX-1…UX-6 integrate, UX-0 integrata, UX-7…UX-10 ancora da fare. |
| X5 | Desktop solo client server: login, libreria remota, browser sync/download API, AI locale e mock e2e | Pending — in corso | Lane Codex avviata sul desktop solo server E7/E10: architettura e implementazione completa, browser sync/download e capacità AI, mockaccount. Pubblicazione resta owner; nessuna parità finale dichiarata. |
| X6 | MCP locale stdio per ricerca, lettura/salvataggio post, cartelle e tag con token scoped | Pending — pronta | Candidati 0e84bf4 + 940bc2d: MCP stdio, 8 tool lettura e 8 scrittura opt-in, UI token TTL/scope. 11 test MCP e 32 UI verdi; review/integrazione e verifica live aperte. |
| X7 | Test avversariale live di auth/scope/CSRF/rate limit/SSRF/upload/injection/IDOR/log leakage | Da fare | Dopo X3; non distruttivo inizialmente, modifiche distruttive solo sul mock, niente DoS/stress del VPS condiviso; report finding e correzioni. |

### Catena AI prioritaria dopo E22

1. **X1 harness:** correggere preflight media/path keyframe e ripetere il benchmark valido; tuning di media/prompt/scorer, con immagini realmente inviate anche per i video.
2. **P3-13:** finire verifica/rebase/commit/integrazione del drain persistente; mantenere schema RawValue F16, offline senza retry persi, pause su quota/chiave, resume/restart e cancellazione.
3. **Release:** suite richieste sul tip con F20, CI remota e deploy reale; configurare operator AI/STT ed allowlist senza esporre chiavi; backup e baseline Hermes.
4. **Gold live corretto:** sanity pass sul nodo, una richiesta per volta e media verificati, riportando qualità per campo e post done/gated/error.
5. **Tagging:** avviare tutti i salvati Instagram del profilo owner richiesti da E22, controllare uno stato pending → analyzing → done ed output persistito, poi monitorare senza duplicare il run. E14 mantiene il perimetro successivo dell’intera libreria, oltre Instagram.
6. **Accettazione:** UI AI/queue e gate di qualità, poi X3 interattivo e X7 sicurezza.

## P0: fondazioni concluse, fuori dalle 115 schede

| ID | Implementazione concreta | Stato | Evidenza o limite residuo |
|---|---|---|---|
| T1 | Workspace Rust, toolchain pin, cargo-deny, deploy e CI | Integrata | `b0baaf5`…`bad7139`. |
| T2 | Legacy reader read-only, identità canoniche e migrate plan | Integrata | `dc6f5e7`…`a4d2c7c`; SPIKE-1 lossless mapping registrato. |
| T3 | Schema v1, UserDb, repository, FTS e golden harness | Integrata | `ec1d742`…`5362bc0`. |
| T4 | SPIKE-5 ricerca/FTS e gate relevance | Integrata | `9f0f4d3`…`37b31d0`; pesi su AI da ritarare in P3-30. |
| T5 | SPIKE-3 estensione MV3 e strumenti confronto | Integrata; prove owner parziali | `a35f4fe`…`6b8dbf4`; X/Pinterest e parity completa restano P2-21/P2-24. |
| T6 | SPIKE-2 CDN e SPIKE-10 tunnel/SSE sul VPS | Integrata | `861e361`, `5e3b64f`; SSE del vero hostname resta P1-26. |
| T7 | Server axum, config, health, metrics, errori, OpenAPI e admin | Integrata | `f95b212`…`bfb0dbc`. |
| T8 | CAS, rendition g480, ThumbHash e serving media | Integrata | `6dd9433`…`1ddb6d9`. |
| T9 | Bundle migrazione, upload resumable ed installazione locale | Integrata | `db57b99`…`567fbf1`; completata da P1-19. |
| T10 | Owner auth magic link, sessioni e CSRF | Integrata | `c7d33ae`…`03a6cf8`; F1 hardening integrato. |
| T11 | Read API, filtri, SSE iniziale e client TypeScript generato | Integrata | `fc1c3b7`…`9b0f643`. |
| T12 | SPA Gallery, ricerca, post e Sidebar sul client comune | Integrata | `0a54f97`…`a1315a0`; desktop failures storici poi corretti da F2/F7. |

- **Gate P0:** exit locale registrato con libreria reale migrata, ricerca e report spike; non dipende da un periodo quotidiano aggiuntivo.
- **Spikes successivi:** SPIKE-4/11 capture/sandbox ed SPIKE-9 video conclusi; SPIKE-8 passkey concluso in P1-24; SPIKE-6 completo ancora P3-10. SPIKE-7 store non richiesto ora per E3 (build unpacked). SPIKE-12/napi/P6 locale è superato dal desktop solo server X5.

## P5: hardening ed uscita finale, fuori dalle 115 schede

P5 non è ancora scomposta in schede numerate; questi requisiti rimangono nel piano §5–§7 e non sono conteggiati come task P1–P4.

| Requisito concreto | Stato | Evidenza già disponibile e parte ancora aperta |
|---|---|---|
| Matrice authz, isolamento utenti, cookie/bearer/scope e deny-by-default | Parziale; gate finale da fare | Test authz presenti; F21 amplia i token, X7 verifica il live e le API finali. |
| ZAP baseline e zero finding high aperti | Da fare | Hardening puntuale integrato, ma nessun gate finale ZAP completo documentato. |
| Audit dipendenze, immagini e binari pin | Parziale; gate finale da fare | Lockfile, cargo-deny/audit e pin yt-dlp/Smokescreen presenti; verifica sul release tip finale. |
| Capacity run e budget end-to-end sul VPS | Da fare | E1 sposta il run sul VPS osn in container limitati; P1-26 e P4-31 misurano budget specifici, non tutto lo scenario §6.3. |
| Chaos: kill -9 durante ingest, capture ed AI | Da fare come gate finale | Test resilienti locali parziali in scheduler/drain; prova finale dei tre percorsi e recovery dati da documentare. |
| Tuning alert, metriche e sette giorni senza alert critici | Parziale; uscita da verificare | Dashboard/backup già presenti; P3-25/P4-28 aggiungono servizi finali. Nessun periodo finale completato registrato. |
| Backup/restore drill sulla configurazione finale e runbook | Parziale | P1 restore reale green; ripetere sull’insieme finale di schemi/servizi e completare runbook. |
| Privacy notice e disclaimer v2 | Da completare | Superfici legali preliminari presenti; testo e comportamento finale coerenti con owner-only e provider consent. |
| Listing Chrome Web Store | Fuori perimetro corrente | E3: estensione unpacked, nessuna submission richiesta ora. |
| Inviti/closed beta 5–10 utenti | Fuori perimetro corrente | E4: owner-only; non avviare inviti per chiudere questo inventario. |
| Parità feature web/desktop e sicurezza finale | Da fare | X3, X5 ed X7 restano lavoro addizionale; nessuna equivalenza tra conteggio task completate e porting finito. |

## Dipendenze e verifiche ancora aperte

| Catena | Perché determina lavoro residuo |
|---|---|
| P4-11 → F21 → X5/X6 | Ordine **CONTROL** 0005_exports prima di 0006_library_tokens; rebase/regenerazione schema e authz, poi client desktop/MCP. Non confondere CONTROL e LIBRARY. |
| P3-06 → P3-04/P3-11; P3-05/P3-06/P3-07 → P3-12 | Explorer/tag/cluster UI ed eval dipendono da dati e ranking ancora incompleti. |
| P3-13/P3-18 → P3-20/P3-24/P3-25/P3-28 | Avere il provider operatore integrato non rende pronta catalogazione, UI, evaluation e deploy. |
| P2-14 + P2-15 → P2-17/P2-18/P2-19 | API task ora completa; worker browser, trigger, controlli web e parità finale ancora aperti. |
| P4-13/P4-14 → P4-23/P4-27/P4-28/P4-30/P4-31 | Servizio capture ed egress presenti, ingest/artifact e container finale da portare nel prodotto/VPS. |
| P4-16/P4-17 → P4-24 | Tool yt-dlp/ffmpeg pronti; cache, keep offline, rimozione ed UI ancora mancanti. |
| P4-10/P4-11 → P4-19/P4-21 | Import/export finale e round trip richiedono entrambe le pipeline. |
| P4-12 → P4-20/P4-26 | Reset e zona pericolosa devono usare purge/GC sicuri e quote corrette. |
| F12 → backfill owner/mock; F8 → deploy → P1-26 | Due implementazioni già concluse conservano azioni operative live esplicitamente aperte. |
| X1 valido → drain/release → tagging Instagram → X3 → X7 | Catena prioritaria dell’owner; baseline caption-only invalidata prima del run completo. |

- **Da verificare sul live:** deploy dei miglioramenti dopo rc.5, backfill F12, provider/STT/allowlist, budget SSE e g480, nuove pipeline capture/video, sync social reali, Android/iOS e pass interattivo finale.
- **Prova di completezza:** 115 ID unici presenti nelle quattro tabelle di fase; F1–F22, UX-0–UX-10, X1–X7 e T1–T12 tutti inclusi separatamente. Nessuna ETA ricavata da vecchi timebox o throughput provvisorio.

## Preflight live della catena tagging (lead, 3 ottobre)

- Nel profilo owner ci sono **3.997 post Instagram non nel Cestino**.
- **2.136** hanno una cover archiviata, **1.790** almeno un oggetto immagine sulle slide, **nessuno** ha un video collegato tramite `post_media.video_object_id`. Questi insiemi si sovrappongono.
- Gli stati archivio Instagram sono **2.255 client** e **1.742 done**; non equivalgono a copertura completa dei media.
- Il gold set ha video scaricati separatamente: il benchmark dei frame non dimostra che la produzione possa usarli. Il lancio deve registrare copertura/attese e completare il percorso media necessario, senza chiamare riuscito un tagging visivo eseguito solo sulle caption.

## Aggiornamento E23 — continuazione fino al completamento

- Il proprietario richiede di continuare fino al 100% del perimetro autorizzato, incluse integrazione, deploy e verifiche reali. Non si chiudono per attestazione verifiche mai eseguite.
- CI del precedente tip7a2adfd: Rust, test Node, web E2E e real-server pass; SSRF falliva per `rg` assente sul runner, corretto in11cb5b9 con identica asserzione `grep -Fx`. Lighthouse LCP4,43s contro2,5s resta F18. Nuova CI da eseguire.
- Configurazione operator AI preparata nel repo osn, commit152598a: variabili compose/Ansible, concurrency1, timeout240s. Validata con Compose sul VPS in sola lettura; URL ancora vuote, nessun deploy.
- Recovery Instagram:1861waiting,81poster recuperabili da copia cache privata; helper admin in lavorazione e P2-17 per rinnovo URL degli altri. Il run owner non è avviato.
- X1: confronto controllato poster8, prompt originale composite0,416; candidato v3 precision0,917 ma composite0,409 e video0,324: gate complessivo non superato. Esperimento caption hashtag conservati come evidenza debole in corso, senza modificare ancora la policy engine. Nessun full40 dichiarato.

- X1 aggiornamento successivo: v3 + caption weak supera confronto poster8 (composite0,454 vs0,416; video0,370 vs0,351; entityF1 0,700 vs0,556). Validazione38complete+2gated avviata seriale; allineamento policy engine autorizzato, full library non avviata.

## Verifiche aggiornate del lead

- Full Rust sullo snapshot 0aa28f9: 102 suite, 1565 pass, 8 ignorati, zero fallimenti. Le integrazioni successive ricevono verifiche mirate; non sono coperte retroattivamente da quel full run.
- CI 37144780896 su 85c37a8: test, rust, read-api-budget, SSRF, web-e2e e web-e2e-live verdi; Lighthouse ancora oltre il budget F18.
- F18: baseline locale LCP 4590 ms; esperimento sulla visibilità immagini 4602 ms, scartato. Chrome DevTools MCP installato e smoke verificato per proseguire la diagnosi.
- Recupero cover: helper admin 24a3368 integrato e 81 poster verificati in bundle privato; nessun apply live. Il conteggio potenziale non è media già recuperata.
- OSN f2d209d prepara configurazione AI operatore, segreto cifrato e runbook; health Ornyth dal VPS verificata. Nessun apply o nuovo deploy. Concorrenza inferenza 1.
- X1: confronto controllato a 8 casi con caption weak migliora composite .416→.454, entity F1 .556→.700. Full40 in corso; non è ancora prova qualitativa completa. Tagging owner non avviato.

### Preparazione rilascio dopo X1

- 970959a pubblicato: pre-push 1731/1731 test verdi. CI 37146987723 avviata; esito non ancora attestato qui.
- 902c44b integra X1; 3a03d2c integra P3-04. Full Rust e typecheck della versione combinata avviati; non ancora conclusi al momento di questo aggiornamento.
- Backup live DB e media eseguiti con Result=success/ExecMainStatus=0; timestamp metriche 1791054349 e 1791054356. Hermes ancora running con zero restart e medesimo avvio del 2 ottobre.
- DevTools MCP ha registrato la Gallery sintetica 6k: LCP osservato 4078 ms, CLS0; immagine LCP da service worker, ritardo render3784 ms. È diagnostica locale sul binario release precedente, non il gate finale Lighthouse del candidato. Nessuna correzione F18 dichiarata.
