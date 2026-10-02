// Golden set for the desktop's `extractContentTerms` (electron/db.ts): the
// tokenizer behind search, ported to crates/core/src/search/terms.rs.
//
// Besides hand-picked queries it covers the two word lists completely: the
// stopwords and the short-term whitelist are read from the source (they are
// not exported) and turned into cases, and the Rust test also checks that its
// lists hold exactly these words.

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { extractContentTerms } from '../../electron/db';
import type { GoldenCase, GoldenSet } from './lib';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

/** The string items of `const <name> = new Set<string>([ ... ]);` in db.ts. */
function setLiteral(source: string, name: string): string[] {
  const start = source.indexOf(`const ${name} = new Set<string>([`);
  if (start < 0) throw new Error(`${name} not found in electron/db.ts`);
  const end = source.indexOf(']);', start);
  const body = source
    .slice(source.indexOf('\n', start), end)
    .split('\n')
    .map((line) => line.replace(/\/\/.*$/, ''))
    .join('\n');
  const words = [...body.matchAll(/'((?:[^'\\]|\\.)*)'/g)].map((m) => m[1]);
  if (words.length === 0) throw new Error(`${name} looks empty`);
  return [...new Set(words)];
}

// [id, query, minLen?]; minLen undefined = the desktop default (3).
const QUERIES: [string, string, number?][] = [
  ['empty', ''],
  ['blank', '  \t\n '],
  ['english', 'minimal desk setup'],
  ['italian', 'lampada da tavolo in vetro'],
  ['mixed-case', 'AirPods Max Review'],
  ['boilerplate-it', 'cerco qualche esempio di poster tipografici'],
  ['boilerplate-en', 'show me some examples of brutalist architecture please'],
  [
    'long-query',
    'Vorrei trovare reference di interior design minimal con legno chiaro e luce naturale per il soggiorno',
  ],
  ['only-stopwords-it', 'di la il per con'],
  ['only-stopwords-en', 'the and of for to'],
  ['short-whitelist', '3d ai ux ui r go vr ar xr cg 2d'],
  ['short-others', 'ok no si io tu x y z 4k'],
  ['duplicates', 'poster Poster POSTER posters poster'],
  ['punctuation', 'c++, node.js & e-mail (draft) — v2.0!'],
  ['underscore', 'snake_case and kebab-case'],
  ['apostrophes', "l'arte dell'architettura e l’estetica"],
  ['digits', '2024 1080p 4k 120fps 3.5mm'],
  ['alphanumeric', 'iphone15 m2pro rtx4090'],
  ['accents-it', 'città perché più già caffè'],
  ['accents-other', 'café crème niño straße über'],
  ['accents-upper', 'CITTÀ ÉTÉ ÜBER'],
  ['turkish', 'İstanbul IŞIK ılık'],
  ['sharp-s', 'STRASSE straße ẞẞẞ'],
  ['greek-sigma', 'ΟΔΟΣ ΟΔΟΣΣ ΣΑΣ Σ'],
  ['cyrillic', 'Москва Дизайн'],
  ['cjk', '東京 デザイン 设计 디자인'],
  ['devanagari', 'डिज़ाइन हिन्दी'],
  ['arabic', 'تَصْمِيم تصميم'],
  ['hebrew', 'עִצּוּב עיצוב'],
  ['thai', 'การออกแบบ'],
  ['emoji', '\u{1F525} fire \u{1F3A8}art \u{1F469}\u200D\u{1F4BB}dev'],
  ['astral-letters', '𝒜𝒜 𝒜 𝔅𝔅𝔅'],
  ['invisible-separators', 'design\u00A0studio\u200Bart\u2028line\uFEFFbom'],
  ['combining-marks', 'cafe\u0301 cafe'],
  ['number-forms', 'Ⅻ ½ ² ⅻⅻⅻ'],
  ['fullwidth', 'ＡＢＣ ｄｅｆ'],
  ['controls', 'tab\there\u0000nul\u001fend'],
  ['min-len-1', 'a di 3d x poster', 1],
  ['min-len-2', 'a di 3d x poster ok', 2],
  ['min-len-4', 'via casa poster ux', 4],
  ['min-len-0', 'a e i o u x', 0],
];

const extractContentTermsSet: GoldenSet = {
  name: 'extract-content-terms',
  source: 'electron/db.ts#extractContentTerms',
  generator: 'scripts/golden/extract-content-terms.ts',
  build(): GoldenCase[] {
    const call = (id: string, query: string, minLen?: number): GoldenCase => {
      const opts = minLen === undefined ? {} : { minLen };
      return { id, args: [query, opts], output: extractContentTerms(query, opts) };
    };
    const cases = QUERIES.map(([id, query, minLen]) => call(id, query, minLen));

    const source = fs.readFileSync(path.join(ROOT, 'electron/db.ts'), 'utf8');
    const stopwords = setLiteral(source, 'SEARCH_STOPWORDS');
    const shortTerms = setLiteral(source, 'SHORT_CONTENT_TERMS');
    for (const w of stopwords) {
      if (extractContentTerms(w, { minLen: 1 }).length) {
        throw new Error(`"${w}" was read as a stopword but is not one: fix setLiteral`);
      }
    }
    // Every stopword, even single letters, is dropped (minLen 1 keeps the rest).
    cases.push(call('stopwords-all', stopwords.join(' '), 1));
    // Every whitelisted short term (the ones that are also stopwords are dropped).
    cases.push(call('short-terms-all', shortTerms.join(' ')));
    // Every 1- and 2-character ASCII token: only whitelisted ones survive.
    const alnum = 'abcdefghijklmnopqrstuvwxyz0123456789'.split('');
    cases.push(call('ascii-1-char', alnum.join(' ')));
    cases.push(call('ascii-2-char', alnum.flatMap((a) => alnum.map((b) => a + b)).join(' ')));
    return cases;
  },
};

export default extractContentTermsSet;
