/* eslint-disable */
//
// Gallery scroll-performance harness
// ──────────────────────────────────
// Misura quanto velocemente le nuove card si caricano nella galleria mentre si
// scrolla, a diverse velocità di scroll. Gira contro la VERA app Electron e la
// TUA libreria reale (nessun mock IPC), così i numeri riflettono la pipeline
// reale: asset:// → generazione thumbnail → decodifica → fade-in.
//
// SCROLL = VERI EVENTI WHEEL (non `scrollTop`). Lo scroll è guidato da
// page.mouse.wheel(), cioè da eventi `mouseWheel` iniettati via CDP nel pipeline
// di input di Chromium. A differenza di `el.scrollTop = …` (mutazione SINCRONA
// sul main thread, che NON riproduce il banding) il wheel passa per il path di
// scroll del COMPOSITOR: il layer scorre in modo asincrono mentre il main thread
// monta le righe — esattamente la desincronizzazione che produce le bande vuote
// percepite con il trackpad. Per questo `el.scrollTop=` "sembrava" fluido.
//
// È un profiler pesante e dipendente dalla macchina, NON un test deterministico:
// è escluso dalla suite e2e normale e va abilitato con PERF=1.
//
//   PERF=1 npx playwright test e2e/perf-gallery.spec.ts --workers=1
//   # oppure:  npm run perf:gallery
//
// MODALITÀ CACHE (env, mutuamente esclusive cold/cacheless):
//   (default)        warm     — cache calda: l'uso reale "a regime". Le immagini
//                               caricano dal disco/HTTP cache quasi istantanee
//                               (instant%), i percentili di latenza sono calcolati
//                               solo sui caricamenti freschi.
//   PERF_COLD=1      cold     — svuota thumb-cache (disco) + cache HTTP di Chromium
//                               PRIMA di ogni profilo, poi si riscalda scorrendo.
//   PERF_CACHELESS=1 cacheless— OGNI tile è sempre rigenerata+decodificata da zero
//                               (bypass thumb-cache su disco via SHELFY_THUMB_NO_CACHE
//                               + cache HTTP via --disable-http-cache + no-store).
//                               Worst case a regime: misura il costo per-tile puro.
//
// ALTRE OPZIONI:
//   PERF_PROFILE=fast  misura un solo profilo (medium|fast|fling).
//   PERF_REPEAT=3      ripete ogni profilo N volte, scarta la prima (warmup) e
//                      riporta la MEDIANA delle restanti (riduce il rumore).
//   PERF_WIN=1440x900  dimensione finestra fissa (default: quasi a tutto schermo).
//
// METRICHE CHIAVE:
//   • mount→load   evento `load` dell'<img> reale. NB: con decoding=async il
//                  browser può emettere `load` PRIMA di aver decodificato il
//                  bitmap → non è il pixel visibile.
//   • mount→decode tempo fino a img.decode() risolta = pixel pronto al paint.
//                  È QUESTA la latenza percepita; cold e cacheless si distinguono
//                  qui (il costo di decodifica è invisibile a `load`).
//   • instant%     card già complete al mount (cache hit): escluse dai percentili
//                  di latenza (le deprimerebbero a ~0), tenute nel cover%.
//   • kept%        tra le card scrollate via, % che aveva l'immagine pronta prima
//                  di uscire (la pipeline tiene il passo a quella velocità).
//   • fps/drop%    SOLO durante lo scroll attivo, jank tarato sul refresh reale
//                  del display (120Hz ProMotion ≠ 60Hz). drop% = frame persi.
//   • banding      px di BANDA VUOTA nel viewport per frame: quanta parte del
//                  viewport, nella direzione di scroll, NON è coperta da righe
//                  renderizzate (il compositor ha scrollato oltre dove il main
//                  thread ha montato). È la misura diretta del bug che percepisci:
//                  band p95/max = ampiezza tipica/peggiore della banda, band% =
//                  % di frame con banda > 4px. Calcolata senza reflow (legge i
//                  translateY inline delle righe, non getBoundingClientRect).
//   • minCard      minimo n° di card piene visibili durante lo scroll: se il
//                  gating placeholder funziona, crolla (placeholder al posto
//                  delle card) e risale a fine scroll.
//
// Il profiler porta la finestra quasi a tutto schermo (vedi PERF_WIN) e disattiva
// prewarm/backfill (PERF_NO_PREWARM) così nessun lavoro di cache in background
// falsa la misura.
// Suggerimento: chiudi l'app Shelfy se è aperta (contesa lock SQLite).
//
// Output: tabella console + perf-results/gallery-<timestamp>.{json,txt} +
// gallery-latest.{json,txt}.

import { test, _electron as electron } from '@playwright/test';
import type { ElectronApplication, Page } from '@playwright/test';
import path from 'path';
import fs from 'fs';
import { execSync } from 'child_process';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.join(__dirname, '..');

const SCHEMA_VERSION = 3;

// ── Tuning ───────────────────────────────────────────────────────────────────
// Finestra del profiler: di default "quasi a tutto schermo" (maximize), perché
// una finestra grande mostra più righe → più card montate/decodificate per
// scroll, che è proprio ciò che incide sulle performance rilevate. PERF_WIN=
// "1440x900" forza invece una dimensione fissa (confronti riproducibili tra
// macchine, a scapito del realismo del carico).
const WIN_OVERRIDE = (() => {
  const m = /^(\d+)x(\d+)$/.exec(process.env.PERF_WIN || '');
  return m ? { width: Number(m[1]), height: Number(m[2]) } : null;
})();
const TARGET_ROWS = 60; // distanza di scroll comune a tutti i profili continuous (in righe)
const COOLDOWN_MS = 700; // drena i job della fase precedente prima di clear/misura
const SETTLE_TIMEOUT_MS = 8000;
const GUARD_FACTOR = 1.6; // margine sulla guardia temporale dello scroll
const GUARD_PAD_MS = 4000;
const GUARD_MAX_MS = 90_000;
const JANK_FACTOR = 1.5; // un frame è "jank" se dura > 1.5× il budget del refresh

// ── Profili di scroll ────────────────────────────────────────────────────────
type Profile =
  | { name: string; kind: 'continuous'; velocity: number }
  | { name: string; kind: 'flings'; count: number; dwellMs: number };

const PROFILES: Profile[] = [
  { name: 'medium', kind: 'continuous', velocity: 2500 },
  { name: 'fast', kind: 'continuous', velocity: 8000 },
  { name: 'fling', kind: 'flings', count: 14, dwellMs: 220 },
];

// ── Strumentazione iniettata nel renderer ────────────────────────────────────
// Definisce window.__perf. Self-contained (nessuna closure esterna): viene
// serializzata e valutata dentro la pagina Electron.
function installPerf() {
  const w = window as any;

  // Il container scrollabile della galleria: il primo antenato del post-grid che
  // scrolla davvero (Gallery possiede lo scroller).
  function findScroller(): any {
    const grid = document.querySelector('[data-testid="post-grid"]');
    let el: any = grid;
    while (el && el !== document.body) {
      const s = getComputedStyle(el);
      if (
        (s.overflowY === 'auto' || s.overflowY === 'scroll') &&
        el.scrollHeight > el.clientHeight + 4
      ) {
        return el;
      }
      el = el.parentElement;
    }
    return grid ? grid.parentElement : null;
  }

  const state: any = {
    scroller: null,
    mo: null,
    rafId: 0,
    sampling: false,
    frames: [] as number[],
    records: [] as any[],
    live: new Map(),
    mountSeq: 0,
    phaseStart: 0,
    tScrollEnd: null,
    reloads: 0,
    unsub: null,
    // Banding: px di banda vuota per frame + n° card piene visibili per frame.
    blanks: [] as number[],
    cardCounts: [] as number[],
    rowH: 0, // altezza riga (px) catturata a beginPhase
    contOff: 0, // offset (px) del contenitore delle righe nello spazio di scroll
  };

  function trackCard(card: any) {
    if (state.live.has(card)) return;
    const idxEl = card.closest('[data-post-index]');
    const rec: any = {
      mountId: ++state.mountSeq,
      index: idxEl ? Number(idxEl.getAttribute('data-post-index')) : -1,
      tMount: performance.now(),
      tImgLoad: null,
      tImgDecoded: null,
      tUnmount: null,
      hadImg: false,
      errored: false,
      instant: false,
    };
    // Selezione POSITIVA della thumbnail (data-testid="card-image" su PostCard):
    // evita di cronometrare una favicon (domain-chip / web fallback) come immagine.
    const img = card.querySelector('[data-testid="card-image"]');
    if (img) {
      rec.hadImg = true;
      const markLoaded = () => {
        if (rec.tImgLoad == null) rec.tImgLoad = performance.now();
      };
      const markDecoded = () => {
        if (rec.tImgDecoded == null) rec.tImgDecoded = performance.now();
      };
      // img.decode() risolve quando il bitmap è decodificato e pronto al paint —
      // ciò che l'utente vede davvero, a differenza dell'evento `load`.
      const tryDecode = () => {
        try {
          img
            .decode()
            .then(markDecoded)
            .catch(() => {});
        } catch {
          /* decode non supportato */
        }
      };
      img.addEventListener('error', () => (rec.errored = true), { once: true });
      if (img.complete && img.naturalWidth > 0) {
        // Già completa al mount = cache hit (memory/HTTP): istantanea.
        rec.instant = true;
        markLoaded();
        tryDecode();
      } else if (img.complete && img.naturalWidth === 0) {
        rec.errored = true;
      } else {
        img.addEventListener(
          'load',
          () => {
            markLoaded();
            tryDecode();
          },
          { once: true },
        );
        // Race: il `load` può essere scattato tra la creazione del nodo (commit
        // React) e l'attach del listener — un tile in cache carica in <1ms.
        if (img.complete && img.naturalWidth > 0) {
          markLoaded();
          tryDecode();
        }
      }
    }
    state.live.set(card, rec);
    state.records.push(rec);
  }

  function untrackCard(card: any) {
    const rec = state.live.get(card);
    if (rec) {
      rec.tUnmount = performance.now();
      state.live.delete(card);
    }
  }

  function collectCards(node: any, out: any[]) {
    if (!node || node.nodeType !== 1) return;
    if (node.matches && node.matches('[data-testid="post-card"]')) out.push(node);
    if (node.querySelectorAll)
      node.querySelectorAll('[data-testid="post-card"]').forEach((c: any) => out.push(c));
  }

  function onMutations(muts: any[]) {
    for (const m of muts) {
      if (m.addedNodes)
        m.addedNodes.forEach((n: any) => {
          const out: any[] = [];
          collectCards(n, out);
          out.forEach(trackCard);
        });
      if (m.removedNodes)
        m.removedNodes.forEach((n: any) => {
          const out: any[] = [];
          collectCards(n, out);
          out.forEach(untrackCard);
        });
    }
  }

  function sampleFrame(now: number) {
    if (!state.sampling) return;
    state.frames.push(now);
    // Banding, SENZA reflow: la copertura verticale delle righe si ricava dai
    // translateY inline (react-virtual posiziona ogni riga con transform), non
    // da getBoundingClientRect (che forzerebbe un layout sincrono per frame e
    // falserebbe gli FPS). Lo spazio coperto = [minY, maxY+rowH] nello spazio di
    // scroll; la banda vuota è la parte di viewport NON coperta nella direzione
    // di scroll. Estremi della lista (top/bottom) esclusi: lì il vuoto è legittimo.
    const el = state.scroller;
    if (el && state.rowH > 0) {
      const scrollTop = el.scrollTop;
      const clientH = el.clientHeight;
      const maxTop = el.scrollHeight - clientH;
      const rows = el.querySelectorAll('[data-index]');
      let blank = 0;
      if (rows.length) {
        let minY = Infinity;
        let maxY = -Infinity;
        rows.forEach((r: any) => {
          const m = /translateY\(([-0-9.]+)px\)/.exec((r.style && r.style.transform) || '');
          const ty = m ? parseFloat(m[1]) : 0;
          if (ty < minY) minY = ty;
          if (ty > maxY) maxY = ty;
        });
        const coveredTop = state.contOff + minY;
        const coveredBot = state.contOff + maxY + state.rowH;
        const atTop = scrollTop <= 2;
        const atBottom = maxTop - scrollTop <= 2;
        const blankTop = atTop ? 0 : Math.max(0, coveredTop - scrollTop);
        const blankBot = atBottom ? 0 : Math.max(0, scrollTop + clientH - coveredBot);
        blank = blankTop + blankBot;
      }
      state.blanks.push(blank);
      state.cardCounts.push(document.querySelectorAll('[data-testid="post-card"]').length);
    }
    state.rafId = requestAnimationFrame(sampleFrame);
  }

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  function stopObservers() {
    state.sampling = false;
    if (state.rafId) {
      cancelAnimationFrame(state.rafId);
      state.rafId = 0;
    }
    if (state.mo) {
      state.mo.disconnect();
      state.mo = null;
    }
    if (state.unsub) {
      try {
        state.unsub();
      } catch {
        /* noop */
      }
      state.unsub = null;
    }
  }

  w.__perf = {
    info() {
      state.scroller = findScroller();
      const el = state.scroller;
      const row = el ? el.querySelector('[data-index]') : null;
      const rowGrid = row ? row.querySelector('.grid') : null;
      const rowHeight = row ? Math.round(row.getBoundingClientRect().height) : 0;
      return {
        found: !!el,
        scrollHeight: el ? el.scrollHeight : 0,
        clientHeight: el ? el.clientHeight : 0,
        scrollable: el ? el.scrollHeight > el.clientHeight + 4 : false,
        rowHeight: rowHeight || (el ? Math.round(el.clientWidth / 6) + 8 : 0),
        cols: rowGrid ? rowGrid.childElementCount : null,
        cards: document.querySelectorAll('[data-testid="post-card"]').length,
        viewport: { w: window.innerWidth, h: window.innerHeight },
        devicePixelRatio: window.devicePixelRatio || 1,
        // Centro dello scroller in coordinate viewport: dove posizionare il
        // cursore prima di iniettare gli eventi wheel (page.mouse.wheel li manda
        // all'elemento sotto al cursore).
        center: el
          ? (() => {
              const r = el.getBoundingClientRect();
              return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) };
            })()
          : { x: Math.round(window.innerWidth / 2), y: Math.round(window.innerHeight / 2) },
      };
    },

    async toTop() {
      if (!state.scroller) state.scroller = findScroller();
      if (state.scroller) state.scroller.scrollTop = 0;
      await sleep(60);
    },

    // Avvia una fase di misura. Idempotente: smonta observer/rAF precedenti.
    // Le card già montate in cima NON sono contate (misuriamo le card che
    // appaiono DURANTE lo scroll); il loro unmount viene ignorato.
    beginPhase() {
      if (!state.scroller) state.scroller = findScroller();
      stopObservers();
      state.records = [];
      state.live = new Map();
      state.frames = [];
      state.blanks = [];
      state.cardCounts = [];
      state.mountSeq = 0;
      state.reloads = 0;
      state.tScrollEnd = null;
      state.phaseStart = performance.now();
      // Cattura una-tantum (a scroll in cima) altezza riga e offset del
      // contenitore delle righe, così il banding si calcola poi senza reflow.
      state.rowH = 0;
      state.contOff = 0;
      const r0 = state.scroller ? state.scroller.querySelector('[data-index]') : null;
      if (r0 && state.scroller) {
        const rr = r0.getBoundingClientRect();
        const sr = state.scroller.getBoundingClientRect();
        const m = /translateY\(([-0-9.]+)px\)/.exec((r0.style && r0.style.transform) || '');
        const ty = m ? parseFloat(m[1]) : 0;
        state.rowH = Math.round(rr.height) || 0;
        state.contOff = rr.top - sr.top + state.scroller.scrollTop - ty;
      }
      state.mo = new MutationObserver(onMutations);
      state.mo.observe(state.scroller, { childList: true, subtree: true });
      state.sampling = true;
      state.rafId = requestAnimationFrame(sampleFrame);
      // Auto-diagnostica: un reload della griglia (es. interceptor:newPosts)
      // rimonta i nodi e falsa i conteggi. Contiamo gli eventi nella fase.
      try {
        const api = (window as any).electronAPI;
        if (api && typeof api.onNewPosts === 'function') {
          state.unsub = api.onNewPosts(() => (state.reloads += 1));
        }
      } catch {
        /* API non disponibile */
      }
    },

    // Geometria di scroll, letta dal driver Node TRA un tick wheel e l'altro per
    // misurare la distanza percorsa e rilevare il fondo lista. Lo scroll vero è
    // guidato da page.mouse.wheel() (vedi spec): qui NON si muta scrollTop, così
    // il movimento passa per il compositor invece che per il main thread.
    scrollState() {
      const el = state.scroller || (state.scroller = findScroller());
      if (!el) return { scrollTop: 0, maxTop: 0, scrollHeight: 0, clientHeight: 0 };
      return {
        scrollTop: el.scrollTop,
        maxTop: Math.max(0, el.scrollHeight - el.clientHeight),
        scrollHeight: el.scrollHeight,
        clientHeight: el.clientHeight,
      };
    },

    // Ferma il campionamento FPS alla FINE dello scroll, così la finestra di
    // settle (idle, fino a 8s) non gonfia l'avgFps né diluisce il jank.
    markScrollEnd() {
      state.tScrollEnd = performance.now();
      state.sampling = false;
      if (state.rafId) {
        cancelAnimationFrame(state.rafId);
        state.rafId = 0;
      }
    },

    async settle(opts: { timeout?: number }) {
      const t0 = performance.now();
      const timeout = opts.timeout || 8000;
      while (performance.now() - t0 < timeout) {
        let pending = 0;
        state.live.forEach((rec: any) => {
          if (rec.hadImg && rec.tImgLoad == null && !rec.errored) pending++;
        });
        if (pending === 0) break;
        await sleep(100);
      }
      return performance.now() - t0;
    },

    endPhase() {
      const reloadsDuringPhase = state.reloads;
      const out = {
        phaseStart: state.phaseStart,
        tScrollEnd: state.tScrollEnd,
        records: state.records.map((r: any) => ({ ...r })),
        frames: state.frames.slice(),
        blanks: state.blanks.slice(),
        cardCounts: state.cardCounts.slice(),
        rowH: state.rowH,
        reloadsDuringPhase,
        scrollTop: state.scroller ? state.scroller.scrollTop : 0,
        scrollHeight: state.scroller ? state.scroller.scrollHeight : 0,
      };
      stopObservers();
      return out;
    },
  };
}

// ── Statistica (lato Node) ───────────────────────────────────────────────────
const round = (v: number | null) => (v == null || Number.isNaN(v) ? null : Math.round(v));

// Percentile nearest-rank: indice ceil(p/100*n)-1, clampato.
function pct(arr: number[], p: number): number | null {
  if (!arr.length) return null;
  const s = [...arr].sort((a, b) => a - b);
  const idx = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
  return s[idx];
}
function maxOf(arr: number[]): number | null {
  return arr.length ? arr.reduce((a, b) => (b > a ? b : a), -Infinity) : null;
}
function median(nums: number[]): number | null {
  const s = nums.filter((n) => typeof n === 'number' && !Number.isNaN(n)).sort((a, b) => a - b);
  if (!s.length) return null;
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : Math.round((s[m - 1] + s[m]) / 2);
}

// Mediana ricorsiva su una serie di aggregati con la stessa forma: numero→mediana,
// oggetto→ricorsione, boolean→OR, stringa/altro→primo.
function mergeMedian(objs: any[]): any {
  const ref = objs[0];
  if (Array.isArray(ref)) return ref;
  if (ref && typeof ref === 'object') {
    const out: any = {};
    for (const k of Object.keys(ref)) out[k] = mergeMedian(objs.map((o) => (o ? o[k] : undefined)));
    return out;
  }
  if (typeof ref === 'number') return median(objs);
  if (typeof ref === 'boolean') return objs.some(Boolean);
  return ref;
}

function aggregate(prof: Profile, data: any, scroll: any, settleMs: number, refreshHz: number) {
  const recs: any[] = data.records;
  const withImg = recs.filter((r) => r.hadImg);
  const live = withImg.filter((r) => !r.errored); // 404/asset mancante esclusi dalle metriche pipeline
  const loaded = live.filter((r) => r.tImgLoad != null);
  const fresh = loaded.filter((r) => !r.instant); // veri caricamenti (no cache hit istantanei)
  const instant = live.filter((r) => r.instant);
  const loadLat = fresh.map((r) => r.tImgLoad - r.tMount);
  const decodeLat = fresh.filter((r) => r.tImgDecoded != null).map((r) => r.tImgDecoded - r.tMount);
  const unmounted = live.filter((r) => r.tUnmount != null);
  const keptUp = unmounted.filter((r) => r.tImgLoad != null && r.tImgLoad <= r.tUnmount);
  const scrolledPastBlank = unmounted.filter((r) => r.tImgLoad == null);
  const stillBlank = live.filter((r) => r.tUnmount == null && r.tImgLoad == null);

  // FPS — solo finestra di scroll (il sampling si ferma a markScrollEnd).
  const f: number[] = data.frames;
  const deltas: number[] = [];
  for (let i = 1; i < f.length; i++) deltas.push(f[i] - f[i - 1]);
  const span = f.length > 1 ? f[f.length - 1] - f[0] : 0;
  const avgFps = span > 0 ? (f.length - 1) / (span / 1000) : null;
  const budget = refreshHz > 0 ? 1000 / refreshHz : 1000 / 60;
  const expected = budget > 0 ? span / budget : 0;
  const dropped = Math.max(0, Math.round(expected) - (f.length - 1));
  const droppedPct = expected > 0 ? Math.round((100 * dropped) / expected) : null;
  const jank = deltas.filter((d) => d > JANK_FACTOR * budget).length;

  // Banding: ampiezza della banda vuota nel viewport (px) campionata per frame.
  const blanks: number[] = data.blanks || [];
  const blankFrames = blanks.filter((b) => b > 4).length;
  const cardCounts: number[] = data.cardCounts || [];
  // Diagnostica: distribuzione delle card piene visibili (per capire se il gate
  // placeholder è STABILE — med≈0 placeholder sempre attivo — o OSCILLA) e
  // numero di frame "lunghi" (>100ms), che sono i veri responsabili dello stutter.
  const longFrames = deltas.filter((d) => d > 100).length;
  const diag = {
    cardMed: median(cardCounts),
    cardMin: cardCounts.length ? Math.min(...cardCounts) : null,
    cardMax: cardCounts.length ? Math.max(...cardCounts) : null,
    longFrames,
    frames: deltas.length,
  };

  return {
    profile: prof.name,
    kind: prof.kind,
    velocityPxS: prof.kind === 'continuous' ? prof.velocity : null,
    cardsMounted: recs.length,
    cardsWithImage: withImg.length,
    errored: withImg.length - live.length,
    imagesLoaded: loaded.length,
    coverPct: live.length ? Math.round((100 * loaded.length) / live.length) : null,
    instantPct: live.length ? Math.round((100 * instant.length) / live.length) : null,
    freshLoads: fresh.length,
    mountToLoadMs: {
      p50: round(pct(loadLat, 50)),
      p95: round(pct(loadLat, 95)),
      max: round(maxOf(loadLat)),
    },
    mountToDecodeMs: {
      p50: round(pct(decodeLat, 50)),
      p95: round(pct(decodeLat, 95)),
      max: round(maxOf(decodeLat)),
    },
    keptUpPct: unmounted.length ? Math.round((100 * keptUp.length) / unmounted.length) : null,
    scrolledPastBlank: scrolledPastBlank.length,
    stillBlankAtEnd: stillBlank.length,
    fps: {
      avg: round(avgFps),
      droppedPct,
      jank,
      frameP95Ms: round(pct(deltas, 95)),
      frameMaxMs: round(maxOf(deltas)),
    },
    banding: {
      p95Px: blanks.length ? round(pct(blanks, 95)) : null,
      maxPx: blanks.length ? round(maxOf(blanks)) : null,
      framesPct: blanks.length ? Math.round((100 * blankFrames) / blanks.length) : null,
      samples: blanks.length,
    },
    minVisibleCards: cardCounts.length ? Math.min(...cardCounts) : null,
    diag,
    scroll: {
      traveledPx: Math.round(scroll.traveled || 0),
      achievedPxS:
        scroll.activeMs && scroll.activeMs > 0
          ? Math.round((scroll.traveled || 0) / (scroll.activeMs / 1000))
          : null,
      truncated: !!scroll.truncated,
      wallMs: Math.round(scroll.wallMs || 0),
    },
    settleMs: Math.round(settleMs),
    reloadsDuringPhase: data.reloadsDuringPhase || 0,
    contaminated: (data.reloadsDuringPhase || 0) > 0,
  };
}

// ── Report leggibile ─────────────────────────────────────────────────────────
function renderTable(headers: string[], rows: any[][]): string {
  const widths = headers.map((h, i) =>
    Math.max(String(h).length, ...rows.map((r) => String(r[i] ?? '').length)),
  );
  const fmt = (cells: any[]) =>
    '  ' +
    cells
      .map((c, i) =>
        i === 0 ? String(c ?? '').padEnd(widths[i]) : String(c ?? '').padStart(widths[i]),
      )
      .join('  ');
  const out = [fmt(headers), '  ' + widths.map((w) => '─'.repeat(w)).join('  ')];
  for (const r of rows) out.push(fmt(r));
  return out.join('\n');
}

const nz = (v: any, suffix = '') => (v == null ? '–' : `${v}${suffix}`);

function buildReport(meta: any, results: any[]): string {
  const L: string[] = [];
  const cacheLabel =
    meta.mode === 'cacheless'
      ? 'SENZA CACHE (ogni tile rigenerata+decodificata — worst case a regime)'
      : meta.mode === 'cold'
        ? 'FREDDA (svuotata per profilo, si riscalda scorrendo)'
        : 'calda (uso reale a regime)';
  L.push('═'.repeat(120));
  L.push('  SHELFY · Gallery scroll performance');
  L.push('═'.repeat(120));
  L.push(`  data:     ${meta.timestamp}   ·   cache: ${cacheLabel}`);
  L.push(
    `  ambiente: ${meta.viewport.w}×${meta.viewport.h} @${meta.devicePixelRatio}x (${meta.windowMode}) · ${meta.cols ?? '?'} colonne · ${meta.refreshHz}Hz · ${meta.platform} · git ${meta.gitSha ?? '?'} · schema v${meta.schemaVersion}`,
  );
  L.push(
    `  metodo:   ${meta.repeats} ripetizion${meta.repeats === 1 ? 'e' : 'i'}/profilo${meta.repeats > 1 ? ' (1ª scartata come warmup, riportata la MEDIANA)' : ''} · target ${meta.targetRows} righe · card iniziali ${meta.initialCards}${meta.scrollable ? '' : ' · NON SCROLLABILE (numeri non significativi)'}`,
  );
  L.push('');

  L.push('  ▸ Caricamento card');
  L.push(
    renderTable(
      [
        'profilo',
        'px/s',
        'montate',
        'con-img',
        'cover%',
        'inst%',
        'load p50',
        'p95',
        'decode p50',
        'p95',
        'max',
        'kept%',
        'blur-pass',
        'vuote',
        'err',
      ],
      results.map((r) => {
        const s = r.summary || {};
        if (s.skipped)
          return [
            r.profile,
            'SKIP',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
            '–',
          ];
        return [
          r.profile,
          s.velocityPxS ?? 'fling',
          nz(s.cardsMounted),
          nz(s.cardsWithImage),
          nz(s.coverPct, '%'),
          nz(s.instantPct, '%'),
          nz(s.mountToLoadMs?.p50),
          nz(s.mountToLoadMs?.p95),
          nz(s.mountToDecodeMs?.p50),
          nz(s.mountToDecodeMs?.p95),
          nz(s.mountToDecodeMs?.max),
          nz(s.keptUpPct, '%'),
          nz(s.scrolledPastBlank),
          nz(s.stillBlankAtEnd),
          nz(s.errored),
        ];
      }),
    ),
  );
  L.push('');

  L.push('  ▸ Fluidità, banding & timing');
  L.push(
    renderTable(
      [
        'profilo',
        'px/s',
        'fps',
        'drop%',
        'jank',
        'frame p95',
        'band p95',
        'band max',
        'band%',
        'minCard',
        'px/s reali',
        'scroll',
        'settle',
      ],
      results.map((r) => {
        const s = r.summary || {};
        if (s.skipped)
          return [r.profile, 'SKIP', '–', '–', '–', '–', '–', '–', '–', '–', '–', '–', '–'];
        return [
          r.profile,
          s.velocityPxS ?? 'fling',
          nz(s.fps?.avg),
          nz(s.fps?.droppedPct, '%'),
          nz(s.fps?.jank),
          nz(s.fps?.frameP95Ms, 'ms'),
          nz(s.banding?.p95Px, 'px'),
          nz(s.banding?.maxPx, 'px'),
          nz(s.banding?.framesPct, '%'),
          nz(s.minVisibleCards),
          nz(s.scroll?.achievedPxS),
          nz(s.scroll?.traveledPx, 'px'),
          nz(s.settleMs, 'ms'),
        ];
      }),
    ),
  );
  L.push('');

  // Avvisi: profili troncati o contaminati
  const flags: string[] = [];
  for (const r of results) {
    const s = r.summary || {};
    if (s.skipped) flags.push(`  ⚠ ${r.profile}: SALTATO (${r.error || 'errore'}).`);
    if (s.contaminated)
      flags.push(
        `  ⚠ ${r.profile}: ${s.reloadsDuringPhase} reload della griglia durante la fase → conteggi inquinati.`,
      );
    if (s.scroll?.truncated)
      flags.push(
        `  ⚠ ${r.profile}: scroll troncato (fondo lista o guardia tempo) prima della distanza target.`,
      );
  }
  if (flags.length) {
    L.push(...flags);
    L.push('');
  }

  L.push('  Legenda:');
  L.push(
    '   • load vs decode  `load` può precedere il bitmap (decoding=async); `decode` è il pixel visibile → la latenza percepita reale.',
  );
  L.push(
    '   • inst%           card già in cache al mount (escluse dai percentili di latenza, incluse nel cover%).',
  );
  L.push(
    "   • cover%          % di card (con immagine, non in errore) che hanno caricato l'immagine entro la fase.",
  );
  L.push(
    '   • kept%           tra le card scrollate via, % già pronta prima di uscire (la pipeline tiene il passo).',
  );
  L.push(
    '   • blur-pass       card scrollate via mostrando solo il blur; vuote = ancora senza immagine a fine fase.',
  );
  L.push(
    '   • fps/drop%/jank  solo durante lo scroll attivo; jank = frame > 1.5× il budget del refresh reale.',
  );
  L.push(
    '   • banding         px di viewport non coperti da righe renderizzate (compositor avanti al main thread).',
  );
  L.push(
    "   • minCard         minimo n° card piene visibili durante lo scroll (crolla se il placeholder s'attiva).",
  );
  L.push(
    '   • px/s reali      velocità effettivamente ottenuta (sotto carico può scendere sotto il nominale).',
  );
  L.push('═'.repeat(120));
  return L.join('\n');
}

// ── Spec ─────────────────────────────────────────────────────────────────────
test.describe('Gallery scroll performance', () => {
  test.skip(!process.env.PERF, 'Profiler opt-in: avvia con PERF=1 (npm run perf:gallery).');
  test.describe.configure({ mode: 'serial' });

  test('latenza caricamento card a diverse velocità di scroll', async () => {
    test.setTimeout(600_000);

    const cold = !!process.env.PERF_COLD;
    const cacheless = !!process.env.PERF_CACHELESS;
    if (cold && cacheless) throw new Error('PERF_COLD e PERF_CACHELESS sono mutuamente esclusivi.');
    const mode = cacheless ? 'cacheless' : cold ? 'cold' : 'warm';
    const repeats = Math.max(1, Math.floor(Number(process.env.PERF_REPEAT) || 1));
    const only = process.env.PERF_PROFILE;
    const profiles = only ? PROFILES.filter((p) => p.name === only) : PROFILES;
    if (!profiles.length) {
      throw new Error(
        `PERF_PROFILE="${only}" non valido. Profili: ${PROFILES.map((p) => p.name).join(', ')}`,
      );
    }

    let gitSha: string | null = null;
    try {
      gitSha = execSync('git rev-parse --short HEAD', { cwd: ROOT }).toString().trim();
    } catch {
      /* fuori da git */
    }

    const app: ElectronApplication = await electron.launch({
      // --disable-http-cache: cacheless deve impedire ogni riuso a livello HTTP
      // Chromium (così ogni GET tile rientra nell'handler e rigenera).
      args: [
        path.join(ROOT, 'electron', 'main.js'),
        ...(cacheless ? ['--disable-http-cache'] : []),
      ],
      cwd: ROOT,
      env: {
        ...process.env,
        ELECTRON_DEV: 'true',
        NODE_ENV: 'test',
        PLAYWRIGHT_E2E: '1',
        // Sempre: un profiler non deve avere prewarm/backfill in background.
        PERF_NO_PREWARM: '1',
        // Bypass della thumb-cache su disco (vedi electron/thumbs.js + main.js).
        ...(cacheless ? { SHELFY_THUMB_NO_CACHE: '1' } : {}),
      },
    });

    if (cacheless) {
      console.warn(
        '[perf-gallery] modalità CACHELESS: ogni tile sempre rigenerata+decodificata (nessuna cache).',
      );
    } else if (cold) {
      console.warn(
        '[perf-gallery] modalità COLD: thumb-cache svuotata e rigenerata a ogni profilo (rigenerabile).',
      );
    }

    try {
      const page: Page = await app.firstWindow();
      await page.waitForLoadState('domcontentloaded');
      // colsPerRow deterministico: forza lo step di densità a 0 prima del bundle.
      await page.addInitScript(() => {
        try {
          localStorage.setItem('gridSizeStep', '0');
        } catch {
          /* noop */
        }
      });
      // Finestra quasi a tutto schermo (maximize) di default, o dimensione fissa
      // se PERF_WIN è impostata. Persiste attraverso i reload cold (stessa window).
      await app.evaluate(({ BrowserWindow }, override) => {
        const win = BrowserWindow.getAllWindows()[0];
        if (!win) return;
        try {
          win.setResizable(true);
          if (override) {
            win.setContentSize(override.width, override.height);
            win.setResizable(false);
          } else {
            win.maximize();
          }
        } catch {
          /* noop */
        }
      }, WIN_OVERRIDE);
      // maximize/resize è asincrona: lascia assestare il layout prima di leggere
      // viewport e colonne.
      await page.waitForTimeout(400);

      const userDataDir: string = await app.evaluate(({ app }) => app.getPath('userData'));
      const refreshHz: number =
        (await app.evaluate(({ screen }) => {
          try {
            return screen.getPrimaryDisplay().displayFrequency || 0;
          } catch {
            return 0;
          }
        })) || 60;

      const prepareReady = async () => {
        await page.waitForSelector('[data-testid="sidebar"]', { timeout: 20_000 });
        const src = page.locator('[data-testid="source-all"]');
        if (await src.count()) await src.click().catch(() => {});
        await page
          .waitForSelector('[data-testid="post-card"]', { timeout: 20_000 })
          .catch(() => {});
      };
      const reloadCold = async () => {
        await page.goto('http://localhost:5173', { waitUntil: 'domcontentloaded' });
        await prepareReady();
        await page.evaluate(installPerf); // il reload azzera il contesto pagina
      };
      const clearCaches = async () => {
        // rmSync della dir intera rimuove anche eventuali .tmp orfani dei rename.
        try {
          fs.rmSync(path.join(userDataDir, 'thumb-cache'), { recursive: true, force: true });
        } catch {
          /* dir assente */
        }
        const res = await app.evaluate(async ({ session }) => {
          try {
            await session.defaultSession.clearCache();
            return true;
          } catch (e: any) {
            return String((e && e.message) || e);
          }
        });
        if (res !== true) console.warn(`[perf-gallery] session.clearCache() non riuscita: ${res}`);
      };

      await page.goto('http://localhost:5173', { waitUntil: 'domcontentloaded' });
      await prepareReady();

      const initialCards = await page.locator('[data-testid="post-card"]').count();
      if (initialCards === 0) {
        console.warn('\n[perf-gallery] La libreria reale è vuota: niente card da misurare.\n');
        test.skip();
        return;
      }

      await page.evaluate(installPerf);
      const info: any = await page.evaluate(() => (window as any).__perf.info());
      if (!info.found) throw new Error('Scroll container della galleria non trovato.');
      if (!info.scrollable) {
        console.warn(
          '\n[perf-gallery] La galleria non è scrollabile (pochi post): numeri non significativi.\n',
        );
      }
      const targetDistance = Math.max(1, info.rowHeight) * TARGET_ROWS;

      const meta = {
        timestamp: new Date().toISOString(),
        schemaVersion: SCHEMA_VERSION,
        mode,
        repeats,
        targetRows: TARGET_ROWS,
        windowMode: WIN_OVERRIDE ? `${WIN_OVERRIDE.width}x${WIN_OVERRIDE.height}` : 'maximize',
        viewport: info.viewport,
        devicePixelRatio: info.devicePixelRatio,
        cols: info.cols,
        rowHeight: info.rowHeight,
        refreshHz,
        initialCards,
        scrollable: info.scrollable,
        platform: process.platform,
        gitSha,
        profiles: profiles.map((p) => p.name),
      };

      // ── Driver di scroll: VERI eventi wheel via CDP (page.mouse.wheel) ─────────
      // Rimpiazzano il vecchio `el.scrollTop = …`: il wheel entra nel pipeline di
      // input di Chromium e scrolla sul thread del COMPOSITOR (asincrono dal main
      // thread), riproducendo il banding del trackpad. Distanza e velocità si
      // misurano leggendo lo scrollTop REALE tra i tick (readScroll), non dai
      // delta inviati (Chromium può coalizzare/animare gli eventi).
      const readScroll = () => page.evaluate(() => (window as any).__perf.scrollState());

      // Scroll continuo a velocità ~costante: ogni giro invio il delta che manca
      // per stare sulla retta velocity×tempo (cap per-evento ≈ realismo trackpad).
      // Riconcilia con lo scrollTop reale ogni ~120ms per distanza e fondo lista.
      const wheelContinuous = async (
        center: { x: number; y: number },
        velocity: number,
        target: number,
        guardMs: number,
      ) => {
        await page.mouse.move(center.x, center.y);
        const t0 = Date.now();
        const maxEvt = Math.max(120, Math.round(velocity / 40));
        let sent = 0;
        let lastReconcile = 0;
        let stuckSince: number | null = null;
        let truncated = false;
        for (;;) {
          const elapsed = (Date.now() - t0) / 1000;
          const delta = Math.min(maxEvt, Math.max(0, Math.round(velocity * elapsed - sent)));
          if (delta > 0) {
            await page.mouse.wheel(0, delta);
            sent += delta;
          } else {
            await page.waitForTimeout(2);
          }
          const tNow = Date.now();
          if (tNow - lastReconcile > 120) {
            lastReconcile = tNow;
            const s = await readScroll();
            if (s.maxTop - s.scrollTop < 2) {
              if (stuckSince == null) stuckSince = tNow;
              else if (tNow - stuckSince > 200) break; // fondo lista
            } else {
              stuckSince = null;
            }
            if (s.scrollTop >= target) break;
          }
          if (tNow - t0 > guardMs) {
            truncated = true;
            break;
          }
        }
        const s = await readScroll();
        return {
          traveled: s.scrollTop,
          activeMs: Date.now() - t0,
          truncated: truncated || s.scrollTop < target - 1,
        };
      };

      // Fling: raffica di delta wheel decrescenti (coda di momentum) + pausa
      // (dwell). jumpPx supera l'overscan così atterra su righe fredde.
      const wheelFlings = async (
        center: { x: number; y: number },
        count: number,
        dwellMs: number,
        jumpPx: number,
      ) => {
        await page.mouse.move(center.x, center.y);
        let truncated = false;
        for (let i = 0; i < count; i++) {
          let remaining = jumpPx;
          let d = Math.min(260, Math.max(80, Math.round(jumpPx / 6)));
          while (remaining > 0) {
            const delta = Math.min(d, remaining);
            await page.mouse.wheel(0, delta);
            remaining -= delta;
            d = Math.max(20, d * 0.9); // decadimento = coda di momentum
          }
          await page.waitForTimeout(dwellMs);
          const s = await readScroll();
          if (s.maxTop - s.scrollTop < 2) {
            truncated = i < count - 1;
            break;
          }
        }
        const s = await readScroll();
        return { traveled: s.scrollTop, activeMs: null, truncated };
      };

      const results: any[] = [];
      for (const prof of profiles) {
        const reps: any[] = [];
        for (let r = 0; r < repeats; r++) {
          try {
            // Drena i job della fase precedente PRIMA di eventuali clear.
            await page.evaluate(() => (window as any).__perf.toTop());
            await page.waitForTimeout(COOLDOWN_MS);
            if (cold) {
              await clearCaches();
              await reloadCold();
            }
            await page.evaluate(() => (window as any).__perf.beginPhase());

            const t0 = Date.now();
            let scroll: any;
            if (prof.kind === 'continuous') {
              const guardMs = Math.min(
                GUARD_MAX_MS,
                (targetDistance / prof.velocity) * 1000 * GUARD_FACTOR + GUARD_PAD_MS,
              );
              scroll = await wheelContinuous(info.center, prof.velocity, targetDistance, guardMs);
            } else {
              // jumpPx oltre l'overscan: ogni fling atterra su righe fredde.
              scroll = await wheelFlings(info.center, prof.count, prof.dwellMs, info.clientHeight * 3);
            }
            scroll.wallMs = Date.now() - t0;
            await page.evaluate(() => (window as any).__perf.markScrollEnd());

            const settleMs: number = await page.evaluate(
              (t) => (window as any).__perf.settle({ timeout: t }),
              SETTLE_TIMEOUT_MS,
            );
            const data: any = await page.evaluate(() => (window as any).__perf.endPhase());
            if (process.env.PERF_DEBUG) {
              try {
                fs.mkdirSync(path.join(ROOT, 'perf-results'), { recursive: true });
                fs.writeFileSync(
                  path.join(ROOT, 'perf-results', `debug-${prof.name}.json`),
                  JSON.stringify({
                    frames: data.frames,
                    blanks: data.blanks,
                    cardCounts: data.cardCounts,
                    rowH: data.rowH,
                  }),
                );
              } catch {
                /* noop */
              }
            }
            reps.push(aggregate(prof, data, scroll, settleMs, refreshHz));
          } catch (e: any) {
            console.warn(
              `[perf-gallery] profilo ${prof.name} ripetizione ${r} fallita: ${e?.message || e}`,
            );
            reps.push({
              profile: prof.name,
              kind: prof.kind,
              skipped: true,
              error: String(e?.message || e),
            });
          }
        }
        const ok = reps.filter((r) => !r.skipped);
        // Scarta la prima ripetizione come warmup quando ne abbiamo più d'una.
        const kept = repeats > 1 && ok.length > 1 ? ok.slice(1) : ok;
        const summary = kept.length
          ? mergeMedian(kept)
          : reps[reps.length - 1] || { profile: prof.name, skipped: true };
        results.push({ profile: prof.name, repeats: reps, kept: kept.length, summary });
        const s: any = summary;
        console.log(
          `[perf-gallery] ${prof.name} (${mode}): mounted ${s.cardsMounted ?? '–'}, cover ${s.coverPct ?? '–'}%, fps ${s.fps?.avg ?? '–'} drop ${s.fps?.droppedPct ?? '–'}% | banding p95 ${s.banding?.p95Px ?? '–'}px max ${s.banding?.maxPx ?? '–'}px | cardVis med ${s.diag?.cardMed ?? '–'} (min ${s.diag?.cardMin ?? '–'}/max ${s.diag?.cardMax ?? '–'}) | frame>100ms ${s.diag?.longFrames ?? '–'}/${s.diag?.frames ?? '–'}${s.contaminated ? ' [CONTAMINATED]' : ''}${s.scroll?.truncated ? ' [TRUNCATED]' : ''}`,
        );
      }

      const report = buildReport(meta, results);
      console.log('\n' + report + '\n');

      const outDir = path.join(ROOT, 'perf-results');
      fs.mkdirSync(outDir, { recursive: true });
      const ts = new Date().toISOString().replace(/[:.]/g, '-');
      const payload = JSON.stringify({ meta, results }, null, 2);
      fs.writeFileSync(path.join(outDir, `gallery-${ts}.json`), payload);
      fs.writeFileSync(path.join(outDir, `gallery-${ts}.txt`), report);
      fs.writeFileSync(path.join(outDir, 'gallery-latest.json'), payload);
      fs.writeFileSync(path.join(outDir, 'gallery-latest.txt'), report);
      console.log(
        `[perf-gallery] risultati salvati in ${path.relative(ROOT, outDir)}/gallery-latest.{json,txt}`,
      );
    } finally {
      await app.close();
    }
  });
});
