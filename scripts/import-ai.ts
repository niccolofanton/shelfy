//
// Import FORZATO dei soli campi AI da un export JSON (es. una ri-analisi fatta su
// un altro PC) nei post LOCALI corrispondenti per `id`. Sovrascrive ai_description/
// ai_tags/ai_entities/ai_keywords/ai_category/ai_content_type/ai_language/
// ai_save_reason/ai_model/ai_status e ricostruisce l'indice post_tags (applicando
// gli alias accettati). NON tocca media, path locali, collezioni.
//
// A differenza dell'import dell'app (bulkUpsert), aggiorna ANCHE i post che hanno
// già un'analisi locale (ai_status='done') — è proprio ciò che serve qui.
//
// USO (CHIUDI PRIMA L'APP, altrimenti conflitto WAL sul DB):
//   cross-env ELECTRON_RUN_AS_NODE=1 electron scripts/import-ai.cjs /percorso/export.json
//   ... aggiungi  --dry-run  per vedere quanti post combaciano senza scrivere nulla.

import fs from 'fs';

const file = process.argv.find((a) => a.endsWith('.json'));
const DRY = process.argv.includes('--dry-run');
if (!file) {
  console.error('Uso: electron scripts/import-ai.cjs <export.json> [--dry-run]');
  process.exit(2);
}
if (!process.versions.electron) {
  console.error('Richiede il node di Electron (ABI better-sqlite3).');
  process.exit(2);
}

// Sotto ELECTRON_RUN_AS_NODE il modulo `electron` non espone `app`. Shim verso la
// userData REALE (così scriviamo nel DB vero, NON una copia scratch).
import os from 'os';
import path from 'path';
import Module from 'module';

const REAL_USERDATA = path.join(os.homedir(), 'Library', 'Application Support', 'Shelfy');

interface ModuleLoader {
  _load(request: string, ...rest: unknown[]): unknown;
}
const ModuleWithLoad = Module as unknown as ModuleLoader;
const _origLoad = ModuleWithLoad._load;
ModuleWithLoad._load = function (this: unknown, request: string, ...rest: unknown[]): unknown {
  if (request === 'electron') {
    return {
      app: {
        getPath: (k: string) => (k === 'userData' ? REAL_USERDATA : os.tmpdir()),
        getName: () => 'Shelfy',
        getVersion: () => '0.0.0',
        isPackaged: false,
      },
    };
  }
  return _origLoad.call(this, request, ...rest);
};

const db = require('../electron/db') as typeof import('../electron/db');
db.initialize();

interface ExportPost {
  id?: unknown;
  aiStatus?: unknown;
  aiTags?: unknown;
  aiDescription?: unknown;
  aiEntities?: unknown;
  aiKeywords?: unknown;
  aiCategory?: unknown;
  aiContentType?: unknown;
  aiLanguage?: unknown;
  aiSaveReason?: unknown;
  aiModel?: unknown;
  aiAnalyzedAt?: unknown;
}

const parsed: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
const posts: ExportPost[] = Array.isArray(parsed)
  ? (parsed as ExportPost[])
  : (parsed as { posts?: ExportPost[] }).posts || [];
console.log(
  `[import-ai] export: ${posts.length} post.${DRY ? '  (DRY RUN — nessuna scrittura)' : ''}`,
);

const str = (v: unknown): string | null =>
  typeof v === 'string' ? v : v == null ? null : String(v);
const arr = (v: unknown): string[] => (Array.isArray(v) ? (v as string[]) : []);

let matched = 0;
let updated = 0;
let missing = 0;
let skipped = 0;

for (const p of posts) {
  const id = p.id != null ? String(p.id) : null;
  if (!id) {
    skipped++;
    continue;
  }
  // Esiste localmente?
  const exists = db.getPost(id);
  if (!exists) {
    missing++;
    continue;
  }
  matched++;
  // Considera solo i post realmente analizzati nell'export.
  if (p.aiStatus !== 'done' && !p.aiTags && !p.aiDescription) {
    skipped++;
    continue;
  }
  if (DRY) {
    updated++;
    continue;
  }

  db.updateAiAnalysis(id, {
    description: p.aiDescription !== undefined ? str(p.aiDescription) : undefined,
    tags: p.aiTags !== undefined ? arr(p.aiTags) : undefined,
    entities: p.aiEntities !== undefined ? arr(p.aiEntities) : undefined,
    keywords: p.aiKeywords !== undefined ? arr(p.aiKeywords) : undefined,
    category: p.aiCategory !== undefined ? str(p.aiCategory) : undefined,
    contentType: p.aiContentType !== undefined ? str(p.aiContentType) : undefined,
    language: p.aiLanguage !== undefined ? str(p.aiLanguage) : undefined,
    saveReason: p.aiSaveReason !== undefined ? str(p.aiSaveReason) : undefined,
    model: p.aiModel !== undefined ? str(p.aiModel) : undefined,
    analyzedAt: typeof p.aiAnalyzedAt === 'number' ? p.aiAnalyzedAt : undefined,
    status: 'done',
  });
  updated++;
}

console.log(
  `[import-ai] combaciati per id: ${matched} | aggiornati: ${updated} | non presenti localmente: ${missing} | saltati: ${skipped}`,
);
console.log(
  DRY
    ? '[import-ai] DRY RUN: nessuna modifica scritta.'
    : "[import-ai] fatto. Tag/keyword aggiornati; tier resta NULL (l'export non porta general/specific).",
);
try {
  db.close();
} catch {}
process.exit(0);
