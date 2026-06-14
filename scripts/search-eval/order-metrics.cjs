'use strict';

// ── P9: metriche d'ORDINE per il retrieval ───────────────────────────────────
//
// Le metriche set-based (precision/recall) esistenti ignorano la POSIZIONE dei
// risultati: trattano la prima pagina come un insieme. Ma la UX reale è una
// lista ordinata, quindi un gold post in posizione 1 vale più che in posizione
// 40. Qui aggiungiamo metriche rank-aware PURE (testabili, senza dipendenze):
//
//   - precisionAtK(ranked, gold, k)  → frazione dei primi k risultati che sono gold
//   - recallAtK(ranked, gold, k)     → frazione del gold catturata nei primi k
//   - mrr(ranked, gold)              → 1/rank del PRIMO gold (0 se nessuno)
//   - ndcgAtK(ranked, gold, k)       → nDCG con rilevanza binaria (gain 1 per gold)
//
// `ranked` = array di id NELL'ORDINE restituito dalla pipeline.
// `gold`   = Set di id rilevanti (oracolo).
//
// Tutte ignorano i duplicati nell'ordine (primo arrivo vince) e restituiscono
// null quando la metrica non è definita (es. gold vuoto), coerentemente con lo
// scoring esistente che usa null per "n/d".

function dedupe(ranked) {
  const seen = new Set();
  const out = [];
  for (const id of ranked || []) {
    if (seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

// p@k: precisione sui primi k risultati restituiti. Denominatore = min(k, |ranked|)
// così non penalizziamo una lista più corta di k (non possiamo restituire ciò
// che non c'è). null se la lista è vuota.
function precisionAtK(ranked, gold, k) {
  const r = dedupe(ranked).slice(0, k);
  if (!r.length) return null;
  const hit = r.filter((id) => gold.has(id)).length;
  return hit / r.length;
}

// r@k: frazione del gold catturata nei primi k. Denominatore = min(|gold|, k)
// (non si può recuperare più gold di quanto ne entri in k slot). null se gold vuoto.
function recallAtK(ranked, gold, k) {
  if (!gold || !gold.size) return null;
  const r = dedupe(ranked).slice(0, k);
  const hit = r.filter((id) => gold.has(id)).length;
  return hit / Math.min(gold.size, k);
}

// MRR: reciproco del rank (1-based) del PRIMO risultato rilevante. 0 se nessun
// gold compare nella lista. null se gold vuoto (metrica non definita).
function mrr(ranked, gold) {
  if (!gold || !gold.size) return null;
  const r = dedupe(ranked);
  for (let i = 0; i < r.length; i++) {
    if (gold.has(r[i])) return 1 / (i + 1);
  }
  return 0;
}

// nDCG@k con rilevanza BINARIA (gain = 1 per gold, 0 altrimenti) e discount
// log2(rank+1). DCG = Σ gain_i / log2(i+2). IDCG = DCG dell'ordinamento ideale
// (tutti i gold disponibili in testa). Ritorna DCG/IDCG ∈ [0,1]. null se gold vuoto.
function ndcgAtK(ranked, gold, k) {
  if (!gold || !gold.size) return null;
  const r = dedupe(ranked).slice(0, k);
  let dcg = 0;
  for (let i = 0; i < r.length; i++) {
    if (gold.has(r[i])) dcg += 1 / Math.log2(i + 2);
  }
  // IDCG: numero ideale di gold posizionabili nei primi k.
  const idealHits = Math.min(gold.size, k);
  let idcg = 0;
  for (let i = 0; i < idealHits; i++) idcg += 1 / Math.log2(i + 2);
  return idcg ? dcg / idcg : null;
}

// Pacchetto completo di metriche d'ordine per un singolo caso.
function orderMetrics(ranked, gold) {
  return {
    'p@5': precisionAtK(ranked, gold, 5),
    'p@10': precisionAtK(ranked, gold, 10),
    'r@5': recallAtK(ranked, gold, 5),
    'r@10': recallAtK(ranked, gold, 10),
    mrr: mrr(ranked, gold),
    'ndcg@10': ndcgAtK(ranked, gold, 10),
  };
}

module.exports = { precisionAtK, recallAtK, mrr, ndcgAtK, orderMetrics, dedupe };
