'use strict';

/* eslint-disable no-console */
//
// Probe NON DISTRUTTIVA per P3 (canonicalizzazione sinonimi): esegue
// analyzer.buildTagAliases sul vocabolario REALE (copia read-only dell'archivio)
// e STAMPA le coppie alias→canonica proposte dal modello. NON chiama
// db.saveTagAliases: nessuna scrittura, l'archivio non viene toccato.
//
//   npm run probe:alias        (o: cross-env ELECTRON_RUN_AS_NODE=1 electron scripts/alias-probe.cjs)

const fs = require('fs');
const os = require('os');
const path = require('path');

if (!process.versions.electron) {
  console.error('[alias-probe] richiede il node di Electron (ABI better-sqlite3). Usa electron, non node.');
  process.exit(2);
}

const REAL_USERDATA = path.join(os.homedir(), 'Library', 'Application Support', 'Shelfy');
const SCRATCH = path.join(__dirname, '.scratch-alias');
const SCRATCH_DB = path.join(SCRATCH, 'shelfy.sqlite');

fs.mkdirSync(SCRATCH, { recursive: true });
for (const suffix of ['', '-wal', '-shm']) {
  const src = path.join(REAL_USERDATA, `shelfy.sqlite${suffix}`);
  const dst = `${SCRATCH_DB}${suffix}`;
  if (fs.existsSync(src)) fs.copyFileSync(src, dst);
  else if (fs.existsSync(dst)) fs.rmSync(dst);
}
// Forza un modello con pesi presenti + symlink dei pesi reali.
fs.writeFileSync(path.join(SCRATCH, 'ai-model.json'), JSON.stringify({ modelId: 'qwen3vl-4b' }));
const realModels = path.join(REAL_USERDATA, 'models');
const linkModels = path.join(SCRATCH, 'models');
try { if (!fs.existsSync(linkModels) && fs.existsSync(realModels)) fs.symlinkSync(realModels, linkModels, 'dir'); } catch {}

// electron shim → punta i moduli di produzione alla scratch dir.
const Module = require('module');
const orig = Module._load;
Module._load = function (request, ...rest) {
  if (request === 'electron') {
    return { app: { getPath: (k) => (k === 'userData' ? SCRATCH : os.tmpdir()), getName: () => 'ShelfyAliasProbe', getVersion: () => '0.0.0', isPackaged: false } };
  }
  return orig.call(this, request, ...rest);
};

async function main() {
  const db = require('../electron/db');
  const analyzer = require('../electron/analyzer');
  db.initialize();

  const unaliased = db.getUnaliasedTags({ limit: 400 }) || [];
  const vocab = db.getCanonicalVocab({ limit: 300 }) || [];
  console.log(`\n[alias-probe] vocabolario: ${unaliased.length} tag senza alias, ${vocab.length} canonici nell'allowlist.`);
  if (!analyzer.getModelStatus().ready) {
    console.error('[alias-probe] modello non pronto — impossibile generare alias.');
    process.exit(1);
  }

  console.log('[alias-probe] genero alias (LLM, può richiedere qualche minuto)…\n');
  const pairs = await analyzer.buildTagAliases({
    onProgress: (p) => { if (p && p.done != null && p.total != null) process.stdout.write(`\r  batch ${p.done}/${p.total}   `); },
  });
  console.log('\n');

  if (!pairs || !pairs.length) {
    console.log('[alias-probe] nessun alias proposto (nessun quasi-sinonimo trovato nel vocabolario).');
  } else {
    console.log(`[alias-probe] ${pairs.length} alias PROPOSTI (NON salvati):\n`);
    for (const p of pairs) console.log(`  "${p.aliasForm}"  →  "${p.canonicalForm}"`);
  }
  console.log('\n[alias-probe] nessuna scrittura effettuata. Archivio reale intatto.\n');

  try { db.close(); } catch {}
  process.exit(0);
}

main().catch((e) => { console.error('[alias-probe] errore:', e && e.stack ? e.stack : e); process.exit(1); });
