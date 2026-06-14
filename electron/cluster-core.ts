// Pure tag-clustering core, shared between db.js (in-process / fallback path and
// unit tests) and cluster-worker.js (worker_threads offload). No DB, no Electron,
// no I/O — only deterministic computation, so it is safe to run on any thread.

// Per-tag frequency map: tag_norm → number of posts carrying it.
type FreqMap = Map<string, number>;

// One raw co-occurrence edge: tags `a` and `b` co-occur in `c` posts.
interface CoEdge {
  a: string;
  b: string;
  c: number;
}

// Optional embedding-based similarity between two tag norms (P4). Injected by
// the caller from the embeddings module; null/absent → jaccard-only weighting.
type CosSim = (a: string, b: string) => number;

// Options accepted by buildTagCommunities (all optional, with defaults below).
interface BuildTagCommunitiesOptions {
  minJaccard?: number;
  maxGroupSize?: number;
  iterations?: number;
  cosSim?: CosSim | null;
  alpha?: number;
}

// A pre-weighted, fused edge kept during community detection (post-jaccard/cos).
interface WeightedEdge {
  a: string;
  b: string;
  w: number;
}

// Cosine similarity between two vectors. Assumes L2-normalized inputs (→ dot
// product) but is robust: returns 0 on empty / mismatched / non-finite, clamped
// to [-1, 1]. Kept byte-for-byte equivalent to embeddings.cosineSim so the
// worker and the in-process fallback produce identical clustering.
function cosineSim(a: number[], b: number[]): number {
  if (!Array.isArray(a) || !Array.isArray(b)) return 0;
  const n = Math.min(a.length, b.length);
  if (n === 0) return 0;
  let dot = 0;
  for (let i = 0; i < n; i++) dot += a[i] * b[i];
  if (!Number.isFinite(dot)) return 0;
  // Clamp: errori in virgola mobile possono dare 1.0000001 anche su vettori unitari.
  return Math.max(-1, Math.min(1, dot));
}

// Pure community detection over a weighted tag graph (no DB access; exported for
// testing). Given per-tag frequencies and the raw co-occurrence edge list, it
// weights edges by Jaccard = c / (freq(a) + freq(b) - c) so high-frequency hubs
// (which co-occur with everything) get LOW weights and stop fusing unrelated
// themes — the core defect of the old connected-components approach.
//
// P4 (embeddings): se viene passata `cosSim(a,b)` (iniettata dal chiamante a
// partire dal modulo embeddings), il peso dell'arco diventa
//   w = alpha·jaccard + (1-alpha)·cos
// così sinonimi che NON co-occorrono possono comunque attrarsi. Senza cosSim
// (fallback solo-jaccard) il comportamento è identico a prima.
//
// P5 (long-tail): NON si tronca più con slice(maxGroupSize) perdendo i tag rari.
// I gruppi troppo grandi vengono SPLITTATI (label propagation ricorsiva interna
// sul sottografo, soglia jaccard alzata) finché non rientrano in maxGroupSize, e
// ogni membro è sempre presente in qualche gruppo (nessun cap silenzioso). Ciò che
// resta comunque scartato (singleton senza vicini) viene loggato dal chiamante.
//
// Deterministic: nodes are processed in a fixed freq-desc order and label ties
// break on the smaller string.
function buildTagCommunities(
  freq: FreqMap,
  edges: CoEdge[],
  {
    minJaccard = 0.15,
    maxGroupSize = 14,
    iterations = 6,
    cosSim = null,
    alpha = 0.5,
  }: BuildTagCommunitiesOptions = {},
): string[][] {
  // Peso d'arco fuso jaccard+cos (o solo-jaccard se cosSim assente).
  const edgeWeight = (a: string, b: string, c: number): number => {
    const fa = freq.get(a) || 0;
    const fb = freq.get(b) || 0;
    const denom = fa + fb - c;
    const j = denom > 0 ? c / denom : 0;
    if (!cosSim) return j;
    let cos = 0;
    try {
      cos = cosSim(a, b);
    } catch {
      cos = 0;
    }
    if (!Number.isFinite(cos)) cos = 0;
    return alpha * j + (1 - alpha) * Math.max(0, cos);
  };

  const byFreqThenName = (x: string, y: string): number =>
    (freq.get(y) || 0) - (freq.get(x) || 0) || (x < y ? -1 : x > y ? 1 : 0);

  // Label-propagation su un sottoinsieme di nodi, usando solo gli archi (≥ thr)
  // interni a quel sottoinsieme. Restituisce Map(label → [norm]).
  const propagate = (
    nodeSet: Set<string>,
    weightedEdges: WeightedEdge[],
    thr: number,
  ): Map<string, string[]> => {
    const adj = new Map<string, Map<string, number>>();
    const link = (a: string, b: string, w: number): void => {
      if (!adj.has(a)) adj.set(a, new Map());
      adj.get(a)!.set(b, w);
    };
    for (const { a, b, w } of weightedEdges) {
      if (w < thr) continue;
      if (!nodeSet.has(a) || !nodeSet.has(b)) continue;
      link(a, b, w);
      link(b, a, w);
    }
    const nodes = [...nodeSet].filter((n) => adj.has(n)).sort(byFreqThenName);
    if (!nodes.length) return new Map();

    const label = new Map<string, string>(nodes.map((n) => [n, n]));
    for (let it = 0; it < iterations; it++) {
      let changed = false;
      for (const n of nodes) {
        const scores = new Map<string, number>();
        for (const [m, w] of adj.get(n)!) {
          const lm = label.get(m)!;
          scores.set(lm, (scores.get(lm) || 0) + w);
        }
        if (!scores.size) continue;
        let best = label.get(n)!;
        let bestScore = -Infinity;
        for (const [lab, s] of scores) {
          if (s > bestScore || (s === bestScore && lab < best)) {
            best = lab;
            bestScore = s;
          }
        }
        if (best !== label.get(n)) {
          label.set(n, best);
          changed = true;
        }
      }
      if (!changed) break;
    }
    const comm = new Map<string, string[]>();
    for (const n of nodes) {
      const l = label.get(n)!;
      if (!comm.has(l)) comm.set(l, []);
      comm.get(l)!.push(n);
    }
    return comm;
  };

  // Pre-pesa tutti gli archi una sola volta; tieni solo quelli sopra minJaccard.
  const weighted: WeightedEdge[] = [];
  const allNodes = new Set<string>();
  for (const { a, b, c } of edges) {
    const w = edgeWeight(a, b, c);
    if (w < minJaccard) continue;
    weighted.push({ a, b, w });
    allNodes.add(a);
    allNodes.add(b);
  }
  if (!allNodes.size) return [];

  const base = propagate(allNodes, weighted, minJaccard);

  // Splitta i gruppi troppo grandi invece di troncarli: rilancia la propagazione
  // sul sottografo del gruppo con soglia jaccard via via più alta, così i temi
  // densi si separano e NESSUN tag raro del gruppo viene scartato (P5).
  const out: string[][] = [];
  const splitQueue: { members: string[]; thr: number }[] = [];
  for (const members of base.values()) {
    if (members.length >= 2) splitQueue.push({ members, thr: minJaccard });
  }
  let guard = 0;
  while (splitQueue.length) {
    if (++guard > 10000) break; // difensivo: niente loop infiniti
    const { members, thr } = splitQueue.shift()!;
    if (members.length <= maxGroupSize) {
      out.push(members);
      continue;
    }
    const nextThr = thr + 0.1;
    const sub = propagate(new Set(members), weighted, nextThr);
    // Se la soglia non separa più nulla (un solo gruppo che copre tutto), spezza
    // deterministicamente per frequenza in chunk di maxGroupSize — così i rari
    // restano comunque raggruppati, mai persi (nessun cap silenzioso).
    const subGroups = [...sub.values()].filter((g) => g.length >= 1);
    const splitProgressed =
      subGroups.length > 1 && subGroups.every((g) => g.length < members.length);
    if (splitProgressed && nextThr <= 1) {
      for (const g of subGroups) {
        if (g.length >= 2) splitQueue.push({ members: g, thr: nextThr });
        else out.push(g); // singleton del sub-split: tienilo comunque (copertura)
      }
    } else {
      const sorted = members.slice().sort(byFreqThenName);
      for (let i = 0; i < sorted.length; i += maxGroupSize) {
        out.push(sorted.slice(i, i + maxGroupSize));
      }
    }
  }

  // Scarta i veri singleton (un solo membro): non formano un gruppo. Tutti gli
  // altri membri sono preservati. Ordine stabile: gruppi più usati prima.
  const groups = out.filter((g) => g.length >= 2).map((g) => g.slice().sort(byFreqThenName));
  const total = (g: string[]): number => g.reduce((s, t) => s + (freq.get(t) || 0), 0);
  groups.sort((A, B) => total(B) - total(A));
  return groups;
}

export { buildTagCommunities, cosineSim };
