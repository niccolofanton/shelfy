'use strict';

// ── P10: scoring del CLUSTERING dei tag (ARI + purity) ───────────────────────
//
// Il clustering produce una PARTIZIONE dei tag (ogni tag in un gruppo). Per
// misurarne la qualità serve un gold: un'altra partizione degli stessi tag,
// annotata a mano (vedi gold.example.json). Confrontiamo predetto vs gold con
// due metriche standard e complementari:
//
//   - ARI (Adjusted Rand Index) ∈ [-1, 1]: accordo tra le due partizioni sulle
//     COPPIE di tag (stesso gruppo / gruppi diversi), corretto per il caso. 1 =
//     identiche, ~0 = come a caso, <0 = peggio del caso. È simmetrico e non si
//     lascia ingannare dal numero di cluster.
//   - purity ∈ [0, 1]: per ogni cluster predetto, quota della sua classe gold
//     maggioritaria, mediata sui tag. Alta e intuitiva, MA NON penalizza il
//     frammentare (mettere ogni tag in un cluster da solo dà purity 1). Per
//     questo va letta INSIEME all'ARI, mai da sola.
//
// Le funzioni sono PURE e testabili. `labels` = mappa tag → etichetta di gruppo
// (string|number). Si valuta sull'INSIEME dei tag presenti in ENTRAMBE le
// partizioni (l'intersezione); la copertura — quanti tag gold sono stati
// effettivamente clusterizzati — è riportata a parte da run.cjs, mai nascosta.

type Label = string | number;
type Labels = Map<string, Label> | Record<string, Label>;

interface Aligned {
  tags: string[];
  pred: Label[];
  gold: Label[];
}

interface Contingency {
  table: Map<string, number>;
  rows: Map<Label, number>;
  cols: Map<Label, number>;
  n: number;
}

interface ClusterScore {
  evaluated: number;
  ari: number | null;
  purity: number | null;
}

function comb2(n: number): number {
  return n < 2 ? 0 : (n * (n - 1)) / 2;
}

// Allinea le due mappe sull'intersezione dei tag e ritorna le etichette
// parallele + la lista dei tag valutati.
function align(predLabels: Labels, goldLabels: Labels): Aligned {
  const tags: string[] = [];
  const pred: Label[] = [];
  const gold: Label[] = [];
  for (const [tag, g] of goldLabels instanceof Map ? goldLabels : Object.entries(goldLabels)) {
    const p = predLabels instanceof Map ? predLabels.get(tag) : predLabels[tag];
    if (p === undefined || p === null) continue; // tag gold non clusterizzato → fuori dall'intersezione
    tags.push(tag);
    pred.push(p);
    gold.push(g);
  }
  return { tags, pred, gold };
}

// Tabella di contingenza n_ij = |cluster predetto i ∩ classe gold j|, più i
// marginali per riga (predetto) e colonna (gold) e il totale n.
function contingency(pred: Label[], gold: Label[]): Contingency {
  const table = new Map<string, number>(); // `${i} ${j}` → conteggio
  const rows = new Map<Label, number>(); // i → conteggio (size cluster predetto)
  const cols = new Map<Label, number>(); // j → conteggio (size classe gold)
  const n = pred.length;
  for (let k = 0; k < n; k++) {
    const i = pred[k];
    const j = gold[k];
    const key = `${i} ${j}`;
    table.set(key, (table.get(key) || 0) + 1);
    rows.set(i, (rows.get(i) || 0) + 1);
    cols.set(j, (cols.get(j) || 0) + 1);
  }
  return { table, rows, cols, n };
}

// Adjusted Rand Index. Casi degeneri (n<2, o entrambe le partizioni triviali —
// tutto in un gruppo o tutti singleton) → 1 se le partizioni coincidono, 0
// altrimenti, coerentemente con la definizione (max==expected).
function adjustedRandIndex(pred: Label[], gold: Label[]): number | null {
  const { table, rows, cols, n } = contingency(pred, gold);
  if (n < 2) return null;

  let sumIJ = 0;
  for (const c of table.values()) sumIJ += comb2(c);
  let sumI = 0;
  for (const c of rows.values()) sumI += comb2(c);
  let sumJ = 0;
  for (const c of cols.values()) sumJ += comb2(c);

  const total = comb2(n);
  const expected = (sumI * sumJ) / total;
  const max = (sumI + sumJ) / 2;
  if (max === expected) return sumIJ === expected ? 1 : 0;
  return (sumIJ - expected) / (max - expected);
}

// Purity: Σ_i max_j n_ij / n. Per ogni cluster predetto prende la classe gold
// più rappresentata e somma quei "voti", normalizzando su n.
function purity(pred: Label[], gold: Label[]): number | null {
  const { table, n } = contingency(pred, gold);
  if (!n) return null;
  const maxByRow = new Map<string, number>(); // i → max_j n_ij
  for (const [key, c] of table.entries()) {
    const i = key.split(' ')[0];
    if (c > (maxByRow.get(i) || 0)) maxByRow.set(i, c);
  }
  let sum = 0;
  for (const v of maxByRow.values()) sum += v;
  return sum / n;
}

// Pacchetto di metriche su una coppia di partizioni (mappe tag→label). Riporta
// anche `evaluated` (tag nell'intersezione) per contestualizzare la copertura.
function scoreClustering(predLabels: Labels, goldLabels: Labels): ClusterScore {
  const { tags, pred, gold } = align(predLabels, goldLabels);
  return {
    evaluated: tags.length,
    ari: adjustedRandIndex(pred, gold),
    purity: purity(pred, gold),
  };
}

export { comb2, align, contingency, adjustedRandIndex, purity, scoreClustering };
export type { Label, Labels, Aligned, Contingency, ClusterScore };
