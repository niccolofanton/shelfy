// Golden set for the desktop's edits of a post's two editable layers
// (electron/db.ts): `updateUserContent` (the note and the manual tags) and
// `updateAiAnalysis` (the AI fields; the manual AI edit calls it with status
// 'done'). Their Rust ports are `update_user_content` and `update_ai` in
// crates/core/src/repo/posts.rs, behind `PATCH /api/v1/posts/{key}`.
//
// Each case starts from a bare post on a fresh desktop library
// (`openDesktopDb()`), applies its steps with the real desktop functions, and
// records the resulting layers: the columns, and the derived tag and entity
// rows. The alias table is part of every case's arguments, so the Rust check
// can rebuild the same state.
//
// Differences from the desktop that the cases stay clear of, on purpose:
// - no case gives a post an AI tag and a manual tag of the same name: the
//   desktop keeps one row for both (its post_tags key has no source), the web
//   keeps two (plan §1.2 #3). Rust tests cover that case.
// - `manualTags: null`: the web API sends `[]` for "no tags".
// - `ai_analyzed_at`: the desktop stamps seconds, the web milliseconds. The
//   clock is pinned and a stamp equal to "now" is recorded as "now".

import type BetterSqlite3 from 'better-sqlite3';
import { openDesktopDb, withDesktopClock, type GoldenCase, type GoldenSet } from './lib';

/** The pinned clock of the run: 2026-10-02T00:00:00Z. */
const NOW_MS = 1_790_899_200_000;

/** `[alias, canonical norm, canonical form, status]` rows of `tag_alias`. */
type Alias = [string, string, string, 'accepted' | 'proposed'];

const ALIASES: Alias[] = [
  ['lampade', 'lampada', 'Lampada', 'accepted'],
  ['ux design', 'ux', 'UX', 'accepted'],
  ['cuffie', 'headphones', 'Headphones', 'accepted'],
  ['vetri', 'vetro', 'Vetro', 'proposed'],
  // The table should hold no chain and no loop; both are followed with a guard.
  ['chain-a', 'chain-b', 'Chain B', 'accepted'],
  ['chain-b', 'chain-c', 'Chain C', 'accepted'],
  ['loop-a', 'loop-b', 'Loop B', 'accepted'],
  ['loop-b', 'loop-a', 'Loop A', 'accepted'],
];

interface UserFields {
  note?: string | null;
  manualTags?: string[];
}

interface AiFields {
  description?: string | null;
  tags?: string[] | null;
  status?: string | null;
  model?: string | null;
  category?: string | null;
  contentType?: string | null;
  entities?: string[] | null;
  keywords?: string[] | null;
  language?: string | null;
  saveReason?: string | null;
  analyzedAt?: number | null;
  generalTags?: string[];
  specificTags?: string[];
}

type Step = { op: 'user'; fields: UserFields } | { op: 'ai'; fields: AiFields };

const user = (fields: UserFields): Step => ({ op: 'user', fields });
const ai = (fields: AiFields): Step => ({ op: 'ai', fields });
/** What the modal's "save" sends (`analyze:updateManual`), in English. */
const manual = (fields: AiFields): Step => ai({ ...fields, status: 'done', model: 'manual' });

const ANALYSIS: AiFields = {
  description: 'A blown-glass table lamp on a wooden desk',
  tags: ['Lampada', 'Glass', 'Design'],
  status: 'done',
  model: 'model-a',
  category: 'interior',
  contentType: 'product',
  entities: ['Murano'],
  keywords: ['blown glass', 'desk lamp'],
  language: 'it',
  saveReason: 'lighting ideas',
  analyzedAt: 1_700_000_000,
  generalTags: ['design'],
  specificTags: ['glass'],
};

const CASES: [string, Step[]][] = [
  // The user layer.
  ['note-set', [user({ note: 'per il soggiorno' })]],
  ['note-as-given', [user({ note: '  Città ✓ — résumé\n\tline two  ' })]],
  ['note-empty-string', [user({ note: 'draft' }), user({ note: '' })]],
  ['note-null-clears', [user({ note: 'draft' }), user({ note: null })]],
  ['note-kept-by-tags', [user({ note: 'keep me' }), user({ manualTags: ['a'] })]],
  ['note-and-tags', [user({ note: 'both', manualTags: ['Lighting', 'Interior Design'] })]],
  ['user-nothing', [user({ note: 'x', manualTags: ['y'] }), user({})]],
  ['tags-dedupe', [user({ manualTags: [' Lamp ', 'lamp', 'LAMP', '', '   ', 'Glass'] })]],
  ['tags-alias', [user({ manualTags: ['Lampade', 'lampada', 'UX design', 'Cuffie'] })]],
  ['tags-alias-proposed', [user({ manualTags: ['Vetri', 'vetro'] })]],
  ['tags-alias-chain-loop', [user({ manualTags: ['Chain-A', 'loop-a', 'LOOP-B'] })]],
  ['tags-replace', [user({ manualTags: ['a', 'b'] }), user({ manualTags: ['c', 'B'] })]],
  ['tags-empty-clears', [user({ manualTags: ['a'] }), user({ manualTags: [] })]],
  [
    'tags-unicode',
    [
      user({
        manualTags: [
          'Città',
          'CITTÀ',
          'ΟΔΟΣ',
          'İstanbul',
          'caf\u00e9',
          'cafe\u0301',
          '\u00a0nbsp\u00a0',
          '\ufeffbom',
          '\u2003em\u3000',
          'STRASSE',
          'straße',
          '日本語',
        ],
      }),
    ],
  ],
  // The AI layer: the manual edit.
  [
    'ai-manual-edit',
    [manual({ description: 'A blown-glass lamp', tags: ['Lamp', 'Glass'], saveReason: 'hall' })],
  ],
  [
    'ai-manual-edit-over-analysis',
    [ai(ANALYSIS), manual({ description: 'Fixed', tags: ['Lamp'], saveReason: '' })],
  ],
  ['ai-manual-edit-empty', [ai(ANALYSIS), manual({ description: '', tags: [], saveReason: '' })]],
  ['ai-manual-edit-aliases', [manual({ tags: ['lampade', 'Lampada', 'LAMPADA', 'cuffie'] })]],
  // The AI layer: the rest of updateAiAnalysis.
  ['ai-analysis', [ai(ANALYSIS)]],
  ['ai-clear-description', [ai(ANALYSIS), ai({ description: null, status: null })]],
  ['ai-clear-tags', [ai(ANALYSIS), ai({ tags: null, status: null })]],
  [
    'ai-tiers',
    [
      ai({
        tags: ['Design', 'lampade', 'Vetri', 'other'],
        generalTags: ['design', 'LAMPADE'],
        specificTags: ['lampada', ' vetri '],
      }),
    ],
  ],
  ['ai-tiers-general-only', [ai({ tags: ['a', 'b'], generalTags: ['a'] })]],
  ['ai-tiers-need-tags', [ai({ tags: ['a'] }), ai({ generalTags: ['a'], description: 'x' })]],
  ['ai-entities', [ai({ entities: ['Murano', 'murano', ' ', 'Tadao Ando', 'lampade'] })]],
  ['ai-entities-cleared', [ai(ANALYSIS), ai({ entities: null })]],
  ['ai-keywords-as-given', [ai({ keywords: ['blown glass', 'Blown Glass', ''] })]],
  ['ai-status-only', [ai({ status: 'analyzing' })]],
  ['ai-error-keeps-stamp', [ai({ status: 'done' }), ai({ status: 'error' })]],
  ['ai-explicit-time', [ai({ status: 'done', analyzedAt: 1_700_000_000 })]],
  ['ai-null-time', [ai({ status: 'done' }), ai({ analyzedAt: null })]],
  ['ai-nothing', [ai(ANALYSIS), ai({})]],
  [
    'ai-unicode',
    [ai({ tags: ['Città', 'città', 'ΟΔΟΣ', ' x '], description: 'Città ✓', language: 'it' })],
  ],
  // Both layers.
  [
    'layers-independent',
    [
      manual({ tags: ['glass'] }),
      user({ manualTags: ['Lighting'] }),
      manual({ tags: [] }),
      user({ note: 'n' }),
    ],
  ],
  ['layers-ai-keeps-user', [user({ manualTags: ['mine'] }), ai(ANALYSIS), ai({ tags: null })]],
];

type Row = Record<string, unknown>;

/** What a case records: a post's two layers after its steps. */
function layers(handle: BetterSqlite3.Database, id: string): unknown {
  const row = handle
    .prepare(
      `SELECT user_note, user_tags, ai_status, ai_model, ai_description, ai_tags, ai_category,
              ai_content_type, ai_entities, ai_keywords, ai_language, ai_save_reason,
              ai_analyzed_at
       FROM posts WHERE id = ?`,
    )
    .get(id) as Row;
  const json = (value: unknown): unknown => (value == null ? null : JSON.parse(String(value)));
  const at = row.ai_analyzed_at as number | null;
  const rows = (sql: string): unknown[] => handle.prepare(sql).raw().all(id);
  return {
    userNote: row.user_note,
    userTags: json(row.user_tags),
    aiStatus: row.ai_status,
    aiModel: row.ai_model,
    aiDescription: row.ai_description,
    aiTags: json(row.ai_tags),
    aiCategory: row.ai_category,
    aiContentType: row.ai_content_type,
    aiEntities: json(row.ai_entities),
    aiKeywords: json(row.ai_keywords),
    aiLanguage: row.ai_language,
    aiSaveReason: row.ai_save_reason,
    aiAnalyzedAt: at === null ? null : at === Math.floor(NOW_MS / 1000) ? 'now' : at,
    manualTagRows: rows(
      "SELECT tag_norm, tag_form FROM post_tags WHERE post_id = ? AND tier = 'manual' ORDER BY tag_norm",
    ),
    aiTagRows: rows(
      `SELECT tag_norm, tag_form, tier FROM post_tags
       WHERE post_id = ? AND (tier IS NULL OR tier IN ('general', 'specific')) ORDER BY tag_norm`,
    ),
    entityRows: rows(
      'SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ? ORDER BY ent_norm',
    ),
  };
}

const editsSet: GoldenSet = {
  name: 'edits',
  source: 'electron/db.ts#updateUserContent,updateAiAnalysis',
  generator: 'scripts/golden/edits.ts',
  build(): GoldenCase[] {
    const { db, sql } = openDesktopDb();
    try {
      return withDesktopClock(sql, NOW_MS, () => {
        const alias = sql.prepare(
          'INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status) VALUES (?, ?, ?, ?)',
        );
        for (const row of ALIASES) alias.run(...row);
        db.invalidateGlobalCaches();
        const insert = sql.prepare("INSERT INTO posts (id, platform) VALUES (?, 'instagram')");
        return CASES.map(([id, steps], n) => {
          const postId = `golden-${n}`;
          insert.run(postId);
          for (const step of steps) {
            if (step.op === 'user') db.updateUserContent(postId, step.fields);
            else db.updateAiAnalysis(postId, step.fields);
          }
          return { id, args: [ALIASES, steps], output: layers(sql, postId) };
        });
      });
    } finally {
      db.close();
    }
  },
};

export default editsSet;
