# extract-eval — harness di valutazione dell'estrazione tag/keyword

Esegue la pipeline di estrazione reale (`electron/analyzer.js`) su un set fisso di
post (`cases.json`) contro un `llama-server` già avviato, poi punteggia tag/keyword
del modello vs l'oracolo curato a mano (`ground-truth.json`) tramite `score.cjs`.

## Come si lancia

```bash
npm run eval:extract                       # tutti i post, score vs ground-truth.json
npm run eval:extract -- --limit=4
npm run eval:extract -- --ids=2042584997689737464,1812372254266724856
npm run eval:extract -- --runs=3           # P11: ri-esegue 3 volte, riporta dispersione
```

Output: tabella in console + `last-report.json`. Senza `ground-truth.json` scrive
invece `last-raw.json` (per autorare il GT).

## P11 — riproducibilità (T=0) e multi-run

L'estrazione VLM è **non deterministica**: senza `temperature 0`, due run sullo
stesso post producono tag diversi, e un singolo shot non è una misura affidabile.

### Riproducibilità: temperature 0
La temperature è **hardcoded** in `electron/analyzer.js` (es. `temperature: 0.2`
nel path di estrazione tag, ~riga 1076). Questo harness NON può sovrascriverla
dall'esterno: l'analyzer non legge un override d'ambiente. Per misure riproducibili:

1. **(consigliato)** porta a `0` la `temperature` del path di estrazione in
   `electron/analyzer.js`. Con T=0 il decoding è greedy/deterministico a parità di
   server e pesi → i run coincidono.
2. In alternativa, stima la dispersione con `--runs=K` (sotto) e ragiona sulla
   mediana, non sul singolo valore.

> Nota di scope: questo harness vive in `scripts/` e non modifica `electron/`. Il
> cambio di temperature va fatto a mano nel codice di produzione (o esposto come
> flag lì) dall'utente.

### --runs=K
Ri-esegue l'INTERA estrazione K volte (default 1 = contratto single-shot
invariato). Il report aggiunge `stability`: per ogni post `{ median, mean,
variance, iqr, min, max, n }` del composito sui K run (via `../lib/agg-stats.cjs`).
IQR alto ⇒ estrazione instabile per quel post (segnale per attivare T=0).

## File
- `run.cjs` — harness. NON editare durante il tuning (è il contratto).
- `score.cjs` — scoring puro (recall/coverage/forbidden/kw/subject → composito).
- `cases.json` — post fissi + frequentTags.
- `ground-truth.json` — oracolo curato a mano (keyed per post id).
- `../lib/agg-stats.cjs` — aggregazione multi-run (P11), condivisa.

## Cosa resta da fornire (utente)
- Decisione su T=0 in `electron/analyzer.js` se serve riproducibilità bit-a-bit.
- Eventuale ampliamento di `ground-truth.json` per nuovi post.
