'use strict';

// Eval cases for the AI-search retrieval loop.
//
// Each case is a free-text query plus a CODE-INDEPENDENT ground truth:
//   - goldTerms : content words (+ synonyms, IT/EN) used to derive the "relevant"
//                 post set directly from the DB (text/ai_description/ai_keywords/
//                 ai_tags LIKE %term%). This is the oracle the harness scores
//                 against — it never touches the production retrieval code.
//   - rejectTags: tags known to be noise for this query; their presence in the
//                 candidate pool is penalised.
//   - kind      : 'hard' = the reported failure (no matching tag exists, must lean
//                 on text/keywords); 'control' = already works, guards regressions.
//   - humanGold : (OPZIONALE, P12) array di POST ID annotati a mano come rilevanti.
//                 Se presente, ha PRECEDENZA su goldTerms: l'oracolo usa esattamente
//                 questi id e NON deriva il set dai campi ai_* (rompe la circolarità,
//                 vedi README.md). goldTerms resta per i casi non ancora annotati.
//
// Keep the set diverse so improvements generalise instead of overfitting to the
// headphones query.
//
// ── P12: ESEMPIO di caso con gold umano (NON popolato, scaffold) ──────────────
// Per annotare un gold non circolare: apri l'app, esegui la query, e raccogli a
// mano gli id dei post DAVVERO rilevanti (giudizio umano, non basato sui tag AI).
// Poi aggiungi qui un caso con `humanGold: ['<postId1>', '<postId2>', …]`.
//   {
//     id: 'cuffie-human',
//     kind: 'hard',
//     query: 'reference per accessori di cuffie come Airpods',
//     // goldTerms ignorati quando humanGold è presente; lasciali come fallback.
//     goldTerms: ['cuffie', 'airpod', 'auricolar'],
//     rejectTags: [],
//     humanGold: [/* '17890000000000000', '17890000000000001' */],
//   },

module.exports = [
  {
    id: 'cuffie',
    kind: 'hard',
    query: 'Devo cercare delle reference per accessori di cuffie come ad esempio Airpods.',
    goldTerms: ['cuffie', 'cuffia', 'airpod', 'auricolar', 'headphone', 'earbud', 'earphone', 'over-ear'],
    rejectTags: ['artefatto', 'artistic', 'digitalart', 'designartistico'],
  },
  {
    id: 'product',
    kind: 'hard',
    query: 'reference di product design per gadget tecnologici e accessori indossabili',
    goldTerms: ['prodotto', 'product', 'gadget', 'wearable', 'indossabil', 'accessori', 'device', 'industrial'],
    rejectTags: [],
  },
  {
    id: 'tipografia',
    kind: 'mid',
    query: 'vorrei trovare delle reference di tipografia animata e cinetica',
    goldTerms: ['typograph', 'tipografia', 'kinetic', 'cinetic', 'font', 'lettering', 'testo animato'],
    rejectTags: [],
  },
  {
    id: 'shader',
    kind: 'control',
    query: 'shader GLSL raymarching',
    goldTerms: ['shader', 'raymarch', 'ray march', 'glsl', 'sdf', 'signed distance'],
    rejectTags: [],
  },
  {
    id: 'fluidi',
    kind: 'control',
    query: 'simulazioni di fluidi e solver',
    goldTerms: ['fluid', 'fluido', 'fluida', 'solver', 'navier', 'simulazione fluid'],
    rejectTags: [],
  },
  {
    // ── Caso AD HOC per P8 (ranking IDF della ricerca solo-tag) ──────────────
    // Costruito per ESERCITARE P8: la sonda solo-tag mescola un tag RARO e
    // distintivo del set gold (touchdesigner, ~3 post) con uno MOLTO comune e
    // incidentale (effetto visivo, ~39 post). Con ORDER BY COUNT i post che
    // portano un solo tag sono in pareggio (→ timestamp, casuale rispetto alla
    // rilevanza); con ORDER BY Σidf il tag raro pesa di più e i post gold
    // (touchdesigner) risalgono. `tagProbeOverride` forza i tag della sonda
    // solo-tag invece di derivarli dai goldTags. Aggiorna i due tag se cambiano
    // le frequenze dell'archivio (vedi README).
    id: 'p8-idf',
    kind: 'control',
    query: 'progetti realizzati con TouchDesigner',
    goldTerms: ['touchdesigner', 'touch designer'],
    rejectTags: [],
    tagProbeOverride: ['touchdesigner', 'effetto visivo'],
  },
];
