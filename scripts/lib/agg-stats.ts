// ── P11: aggregazione multi-run ──────────────────────────────────────────────
//
// Gli harness LLM non sono deterministici: una singola esecuzione è rumore. Con
// --runs=K eseguiamo ogni caso K volte e riportiamo la TENDENZA CENTRALE (mediana,
// robusta agli outlier) e la DISPERSIONE (varianza + IQR) per ogni metrica.
//
// Funzioni pure e testabili: operano su array di numeri (i valori della stessa
// metrica attraverso i run). I null/undefined/NaN vengono scartati (metrica n/d
// in quel run) — se non resta nulla, le statistiche sono null.

type MaybeNumber = number | null | undefined;

interface MetricSummary {
  n: number;
  median: number | null;
  mean: number | null;
  variance: number | null;
  iqr: number | null;
  min: number | null;
  max: number | null;
}

function clean(values: readonly MaybeNumber[] | null | undefined): number[] {
  return (values || []).filter((v): v is number => typeof v === 'number' && Number.isFinite(v));
}

function median(values: readonly MaybeNumber[] | null | undefined): number | null {
  const v = clean(values)
    .slice()
    .sort((a, b) => a - b);
  if (!v.length) return null;
  const mid = Math.floor(v.length / 2);
  return v.length % 2 ? v[mid] : (v[mid - 1] + v[mid]) / 2;
}

function mean(values: readonly MaybeNumber[] | null | undefined): number | null {
  const v = clean(values);
  return v.length ? v.reduce((a, b) => a + b, 0) / v.length : null;
}

// Varianza di popolazione (divisore N). Per K piccolo è una stima grezza ma
// sufficiente a segnalare instabilità tra i run.
function variance(values: readonly MaybeNumber[] | null | undefined): number | null {
  const v = clean(values);
  if (!v.length) return null;
  const m = v.reduce((a, b) => a + b, 0) / v.length;
  return v.reduce((a, b) => a + (b - m) ** 2, 0) / v.length;
}

// Quantile con interpolazione lineare (metodo "linear"/R-7).
function quantile(values: readonly MaybeNumber[] | null | undefined, q: number): number | null {
  const v = clean(values)
    .slice()
    .sort((a, b) => a - b);
  if (!v.length) return null;
  if (v.length === 1) return v[0];
  const pos = (v.length - 1) * q;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  if (lo === hi) return v[lo];
  return v[lo] + (pos - lo) * (v[hi] - v[lo]);
}

// IQR = Q3 − Q1: dispersione robusta, immune a un singolo run anomalo.
function iqr(values: readonly MaybeNumber[] | null | undefined): number | null {
  const q1 = quantile(values, 0.25);
  const q3 = quantile(values, 0.75);
  return q1 == null || q3 == null ? null : q3 - q1;
}

// Riassume una metrica sui K run: { median, mean, variance, iqr, min, max, n }.
function summarize(values: readonly MaybeNumber[] | null | undefined): MetricSummary {
  const v = clean(values);
  return {
    n: v.length,
    median: median(v),
    mean: mean(v),
    variance: variance(v),
    iqr: iqr(v),
    min: v.length ? Math.min(...v) : null,
    max: v.length ? Math.max(...v) : null,
  };
}

// Dato un array di oggetti-metriche (uno per run), tutti con le STESSE chiavi,
// restituisce { chiave → summarize([valori della chiave nei run]) }.
function summarizeRuns(
  runMetrics: ReadonlyArray<Record<string, MaybeNumber> | null | undefined> | null | undefined,
): Record<string, MetricSummary> {
  const out: Record<string, MetricSummary> = {};
  const keys = new Set<string>();
  for (const m of runMetrics || []) for (const k of Object.keys(m || {})) keys.add(k);
  for (const k of keys) {
    out[k] = summarize((runMetrics || []).map((m) => (m ? m[k] : null)));
  }
  return out;
}

export { median, mean, variance, quantile, iqr, summarize, summarizeRuns, clean };
