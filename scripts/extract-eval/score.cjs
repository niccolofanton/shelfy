// Scoring contract for the extraction harness. Compares the local model's
// tags/keywords against a human/Claude-curated ground-truth oracle.
//
// Per-post ground truth shape (ground-truth.json, keyed by post id):
//   { subject: "cuffie",                // the one concrete subject term (in mustHave too)
//     mustHave:   ["cuffie","airpods"], // core tags that MUST appear (recall-critical)
//     acceptable: ["audio","gadget"],   // additional reasonable tags (no penalty)
//     forbidden:  ["shader","generative"], // wrong/generic tags that MUST NOT appear
//     idealKeywords: ["airpods accessori","custodia auricolari"] }
//
// Philosophy mirrors the search-eval harness: reward recall of the curated
// must-haves + coverage on the allow-list, reward capturing the concrete
// subject, and heavily penalize forbidden/off-topic tags. Fuzzy matching
// (stem + compact + token-subset + synonyms) avoids trivial string misses.

// IT↔EN domain equivalences — tags legitimately appear in either language
// ("i tag tecnici restano nella forma comune del settore, anche inglese"), so
// the matcher must treat translations as the same concept. General domain terms,
// not post-specific phrases.
const SYNONYMS = [
  ['3d', 'tridimensionale', 'cgi'],
  ['motion design', 'motion graphics', 'motiongraphics'],
  ['tipografia', 'typography', 'type', 'lettering', 'typeface'],
  ['illustrazione', 'illustration', 'illustrazioni'],
  ['shader', 'shaders', 'glsl shader'],
  ['fluidi', 'fluid', 'fluid simulation', 'fluid solver', 'simulazione fluidi'],
  ['cuffie', 'cuffia', 'headphones', 'headphone', 'auricolari', 'earbuds', 'earphones'],
  ['scultura', 'sculpture', 'sculptures', 'sculture'],
  ['moda', 'fashion', 'abbigliamento'],
  ['prodotto', 'product', 'product design', 'design di prodotto'],
  ['ritratto', 'portrait', 'portraits'],
  ['disegno', 'drawing', 'sketch', 'schizzo', 'figure drawing', 'studio di figura', 'figure study'],
  ['creative coding', 'creativecoding', 'coding creativo'],
  ['generative', 'generativa', 'generativo', 'arte generativa', 'generative art'],
  // cross-language domain terms surfaced by the archive
  ['terreno', 'terrain'],
  ['procedurale', 'procedural'],
  ['vettoriale', 'vector', 'vettore', 'vector graphics', 'grafica vettoriale'],
  ['nuvola di punti', 'point cloud', 'pointcloud'],
  ['scansione 3d', '3d scanning', '3d scan', 'scan 3d'],
  ['fotogrammetria', 'photogrammetry'],
  ['visualizzazione dati', 'data visualization', 'data viz', 'dataviz', 'datavis'],
  ['grafo a nodi', 'node graph', 'node-graph', 'graph'],
  ['app musicale', 'music app', 'music network'],
  ['sfera di vetro', 'glass orb', 'glass sphere'],
  ['react three fiber', 'r3f'],
  ['pittura digitale', 'digital painting'],
  ['cibo finto', 'fake food', 'food replica', 'replica alimentare', 'sanpuru'],
  ['aerografo', 'airbrush', 'aerografia'],
  ['annuncio di lavoro', 'job posting', 'job ad', 'hiring', 'wanted'],
  ['asset di gioco', 'game assets', 'game asset', 'gameassets'],
  ['arte di strada', 'street art', 'arte urbana'],
  ['scultura urbana', 'street sculpture', 'urban sculpture'],
  ['interfaccia', 'ui', 'ui design', 'interface', 'user interface', 'interfaccia utente'],
  ['mostra', 'esposizione', 'exhibition'],
  ['logotipo', 'logo', 'wordmark'],
  ['cavaliere', 'knight'],
  ['open source', 'opensource'],
];

function norm(t) {
  return String(t || '')
    .toLowerCase().trim()
    .replace(/^#+/, '')
    .replace(/[_-]+/g, ' ')
    .replace(/[^\p{L}\p{N} ]+/gu, '')
    .replace(/\s+/g, ' ')
    .trim();
}
const compact = (t) => norm(t).replace(/\s+/g, '');
const stem = (t) => norm(t).split(' ').map((w) => (w.length > 4 ? w.replace(/(s|i|e)$/u, '') : w)).join(' ');

function synGroup(t) {
  const n = norm(t);
  for (const g of SYNONYMS) if (g.some((x) => norm(x) === n)) return g.map(norm);
  return [n];
}

// True if model tag `a` matches ground-truth tag `b` (fuzzy, symmetric-ish).
function matchTag(a, b) {
  const na = norm(a), nb = norm(b);
  if (!na || !nb) return false;
  if (na === nb) return true;
  if (compact(a) === compact(b)) return true;
  if (stem(a) === stem(b)) return true;
  // token-subset: all tokens of the shorter phrase appear in the longer
  const ta = na.split(' '), tb = nb.split(' ');
  const [short, long] = ta.length <= tb.length ? [ta, tb] : [tb, ta];
  if (short.length >= 1 && short.every((w) => long.includes(w)) && (short.length > 1 || short[0].length >= 4)) return true;
  // synonyms
  const ga = synGroup(a);
  if (ga.includes(nb) || synGroup(b).includes(na)) return true;
  return false;
}

const matchesAny = (tag, list) => (list || []).some((g) => matchTag(tag, g));

const STOP = new Set(['di', 'a', 'da', 'in', 'con', 'su', 'per', 'tra', 'fra', 'e', 'il', 'lo', 'la', 'i', 'gli', 'le', 'un', 'uno', 'una', 'the', 'of', 'and', 'to', 'with', 'come', 'cosa', 'che', 'del', 'della', 'dei', 'delle']);
const contentTokens = (s) => norm(s).split(' ').filter((w) => w.length >= 3 && !STOP.has(w));

// Compositional recall: a multi-word ground-truth term counts as captured if a
// pool tag matches it directly, OR a majority of its content tokens are each
// covered by some pool tag (credits "terrain"+"procedural" → "terreno procedurale").
function tokenCovered(token, pool) {
  return pool.some((p) => matchTag(p, token) || p.split(' ').some((w) => matchTag(w, token)));
}
function matchConcept(term, pool) {
  if (pool.some((p) => matchTag(p, term))) return true;
  const toks = contentTokens(term);
  if (toks.length < 2) return false;
  const covered = toks.filter((t) => tokenCovered(t, pool)).length;
  return covered / toks.length >= 0.6;
}

// Forbidden match is directional+strict: flag a model tag only if it IS the
// forbidden concept (equal/compact/stem/synonym) or a SUPERSET of it (all
// forbidden tokens ⊆ model tokens). Never flag a model tag that is merely a
// subset of a multi-word forbidden term — so "coding" ≠ forbidden "creative coding".
function matchForbidden(modelTag, forbiddenTag) {
  const na = norm(modelTag), nb = norm(forbiddenTag);
  if (!na || !nb) return false;
  if (na === nb || compact(modelTag) === compact(forbiddenTag) || stem(modelTag) === stem(forbiddenTag)) return true;
  if (synGroup(modelTag).includes(nb) || synGroup(forbiddenTag).includes(na)) return true;
  const tModel = na.split(' '), tF = nb.split(' ');
  if (tF.every((w) => tModel.includes(w))) return true; // model tag ⊇ forbidden term
  return false;
}
const matchesAnyForbidden = (tag, list) => (list || []).some((g) => matchForbidden(tag, g));

function scoreCase(gt, out) {
  const tags = (out.tags || []).map(norm).filter(Boolean);
  const entities = (out.entities || []).map(norm).filter(Boolean);
  const kws = out.keywords || [];
  const desc = out.description || '';
  // Concepts can be captured as either a tag or an entity (proper nouns —
  // brands/authors/tools — belong in `entities` per the prompt). Recall and
  // subject capture are scored over the union; coverage/forbidden stay on tags
  // only, to keep the curated tag set clean.
  const recallPool = [...tags, ...entities];

  const mustHave = gt.mustHave || [];
  const matchedMust = mustHave.filter((g) => matchConcept(g, recallPool));
  const recall = mustHave.length ? matchedMust.length / mustHave.length : 1;

  const onSet = tags.filter((t) => matchesAny(t, mustHave) || matchesAny(t, gt.acceptable));
  const forbiddenHits = tags.filter((t) => matchesAnyForbidden(t, gt.forbidden));
  const coverage = tags.length ? onSet.length / tags.length : 0;
  const forbiddenRate = tags.length ? forbiddenHits.length / tags.length : 0;

  // subject present anywhere the user could find it (tags/entities preferred)
  const subj = gt.subject ? [gt.subject] : mustHave.slice(0, 1);
  const subjectInTags = subj.length ? subj.some((s) => matchConcept(s, recallPool)) : true;
  const subjInText = subj.length ? contentTokens(subj.join(' ')).some((w) => norm(desc).includes(w) || kws.some((k) => norm(k).includes(w))) : false;
  const subjectHit = subjectInTags ? 1 : (subjInText ? 0.4 : 0);

  // keyword relevance: ideal keyword content tokens captured by model keywords/desc/tags
  const ideal = gt.idealKeywords || [];
  let kwScore = 1;
  if (ideal.length) {
    const haystack = norm([...(kws || []), desc, ...tags].join(' '));
    const hit = ideal.filter((ik) => { const toks = contentTokens(ik); return toks.length && toks.some((w) => haystack.includes(w)); });
    kwScore = hit.length / ideal.length;
  }

  const composite = Math.max(0, Math.min(1,
    0.45 * recall + 0.25 * coverage + 0.15 * kwScore + 0.15 * subjectHit - 0.50 * forbiddenRate));

  return {
    recall: +recall.toFixed(3),
    coverage: +coverage.toFixed(3),
    forbiddenRate: +forbiddenRate.toFixed(3),
    kwScore: +kwScore.toFixed(3),
    subjectHit: +subjectHit.toFixed(2),
    composite: +composite.toFixed(3),
    matchedMust, missedMust: mustHave.filter((g) => !matchedMust.includes(g)),
    forbiddenHits: forbiddenHits,
    tags,
  };
}

module.exports = { norm, matchTag, matchesAny, scoreCase, SYNONYMS };
