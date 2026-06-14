# search-eval — harness di valutazione del retrieval AI

Strumento di misura STABILE per il loop di miglioramento di prompt/retrieval.
Esercita il codice di produzione reale (`electron/db.js` + `electron/analyzer.js`)
contro una copia READ-ONLY dell'archivio e valuta tre pilastri: **TAGS**,
**KEYWORDS**, **RESULTS**.

## Come si lancia

```bash
npm run eval:search                 # loop deterministico (veloce, niente LLM)
npm run eval:search -- --llm        # esegue anche il modello reale end-to-end
npm run eval:search -- --case=cuffie
npm run eval:search -- --runs=5     # P11: 5 run/caso, riporta mediana + dispersione
```

Output: tabella in console + `last-report.json` (machine-readable).

## Metriche

### Set-based (storiche)
- `searchPrecision`, `searchRecall` — sull'INSIEME dei risultati di prima pagina.
- `poolRelevance`, `poolNoise`, `keywordRelevance` — qualità di tag/keyword.

### P9 — metriche d'ORDINE (nuove, affiancate)
Calcolate sull'ORDINE reale dei risultati restituiti dalla pipeline vs il gold
(non solo sull'insieme). Implementate in `order-metrics.cjs` (pure, testabili):
- `p@5`, `p@10` — precisione nei primi k.
- `r@5`, `r@10` — recall nei primi k.
- `mrr` — reciproco del rank del primo gold.
- `ndcg@10` — nDCG a rilevanza binaria, discount log2.

Compaiono in `metrics` di ogni caso in `last-report.json`, accanto alle storiche.

### P11 — multi-run non deterministico
`--runs=K` (default 3) esegue ogni caso K volte. Il report `last-report.json`
include `runsAgg`: per ogni metrica `{ median, mean, variance, iqr, min, max, n }`
(vedi `../lib/agg-stats.cjs`). La mediana è la tendenza centrale robusta; IQR/
varianza misurano l'instabilità tra i run. Con la pipeline deterministica i K run
coincidono (varianza ≈ 0); il flag serve soprattutto con `--llm` o pipeline future
non deterministiche.

## P12 — Caveat sulla CIRCOLARITÀ del gold (importante)

Il gold di default (`goldTerms` in `cases.cjs`) deriva l'insieme rilevante da
`text` + `ai_description` + `ai_keywords` + `ai_tags` via `LIKE %term%`. Ma sono
gli **stessi campi** che la pipeline di retrieval usa per cercare → la misura è
**parzialmente circolare**: stiamo valutando il retrieval contro un gold prodotto
dallo stesso meccanismo. Un retrieval che imita i campi ai_* sembra "bravo" anche
se non capisce la query.

### Gold ALTERNATIVO (umano), non circolare
Lo scaffold è già predisposto. Ogni caso può portare un campo opzionale
`humanGold: ['<postId>', …]`. **Se presente, ha precedenza** su `goldTerms`:
l'oracolo usa esattamente quegli id e non consulta i campi ai_* per derivare il
set. Il report marca la sorgente con `goldSource: 'human' | 'ai-derived'`.

Come popolarlo (giudizio umano, da fornire):
1. Apri l'app ed esegui la query del caso.
2. Scorri i risultati e annota gli ID dei post DAVVERO rilevanti, secondo il tuo
   giudizio — NON basandoti sui tag/descrizioni AI.
3. Incolla gli id in `humanGold` del caso corrispondente in `cases.cjs` (c'è un
   esempio commentato `cuffie-human`).

Finché `humanGold` resta vuoto, i casi usano il gold ai-derived e il report lo
dichiara esplicitamente.

## File
- `run.cjs` — harness (electron-as-node, DB copia read-only, scrive last-report.json). NON editare durante il tuning.
- `cases.cjs` — casi + ground truth (incl. scaffold `humanGold`).
- `order-metrics.cjs` — metriche d'ordine pure (P9).
- `../lib/agg-stats.cjs` — aggregazione multi-run pura (P11), condivisa con cluster-eval.
