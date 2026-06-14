# cluster-eval (P10)

Harness di valutazione del **clustering dei tag** prodotto da `electron/db.js`
(`getTagCandidateGroups`: co-occorrenza Jaccard + embeddings opzionali). Misura
la partizione predetta contro un **gold annotato a mano**, con **ARI** e
**purity**. Senza questo, ogni modifica al clustering è alla cieca.

## Metriche

- **ARI** (Adjusted Rand Index) ∈ [-1, 1]: accordo sulle COPPIE di tag (stesso
  gruppo / gruppi diversi), corretto per il caso. 1 = identico al gold, ~0 = come
  a caso. È la metrica primaria: simmetrica, non ingannata dal numero di cluster.
- **purity** ∈ [0, 1]: quota della classe gold maggioritaria per cluster, mediata
  sui tag. Intuitiva ma **NON penalizza la frammentazione** (un tag per cluster →
  purity 1). Va letta SEMPRE insieme all'ARI.
- **copertura**: quota dei tag gold che il clustering ha effettivamente messo in
  un gruppo (gli altri sono esclusi dall'ARI — riportato, mai nascosto).

## Come preparare il gold

1. Copia `gold.example.json` in `gold.json` (nella stessa cartella).
2. Sostituisci i cluster placeholder con la partizione REALE dei tag del tuo
   archivio. Formato: `{ "clusters": [ { "label": "...", "tags": ["norm", ...] } ] }`.
   - `tags` in **tag_norm** (minuscolo, come `post_tags.tag_norm`).
   - Ogni tag in **un solo** cluster.
   - Non serve coprire TUTTI i tag: quelli non elencati non sono valutati (ma
     abbassano la copertura riportata). Conviene annotare i tag più frequenti.
3. `gold.json` è gitignored-by-convention (dato dell'utente); `gold.example.json`
   resta come schema di riferimento.

## Lancio

```bash
npm run eval:cluster                 # punteggia contro ./gold.json
npm run eval:cluster -- --runs=5     # K esecuzioni → mediana + dispersione (IQR/var)
npm run eval:cluster -- --gold=/path/to/gold.json
```

Gira sotto il node di Electron (ABI di better-sqlite3). Copia `shelfy.sqlite` in
`.scratch/` e ci punta via shim `electron`: **non** muta mai l'archivio reale. Se
è installato un modello di embedding, il codice lo usa come in produzione;
altrimenti ricade su solo-Jaccard (entrambi i path sono valutabili). Scrive
`last-report.json`.

## Note

- `getTagCandidateGroups` è quasi deterministico; `--runs>1` serve soprattutto a
  cogliere la varianza introdotta dagli embeddings e a validare l'aggregazione
  multi-run condivisa (`scripts/lib/agg-stats.cjs`).
- Lo scoring puro (`score.cjs`: ARI, purity, contingenza) è testabile in
  isolamento, senza DB né Electron.
