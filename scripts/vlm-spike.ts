// Spike di fattibilità: llama-server (Qwen3-VL-4B + mmproj) + ffmpeg keyframe →
// {description, tags} in italiano. Standalone, fuori dal bundle dell'app.
//
// Uso: node scripts/vlm-spike.mjs [path/al/video.mp4]

import { spawn, spawnSync } from 'node:child_process';
import { createServer, type AddressInfo } from 'node:net';
import { readFileSync, existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import path from 'node:path';

const REPO = path.resolve(import.meta.dirname, '..');
const BIN = path.join(REPO, '.vlm/llama-b9500/llama-server');
const MODELS = path.join(homedir(), 'Library/Application Support/shelfy/models');
const MODEL = path.join(MODELS, 'Qwen3VL-4B-Instruct-Q4_K_M.gguf');
const MMPROJ = path.join(MODELS, 'mmproj-Qwen3VL-4B-Instruct-F16.gguf');
const FFMPEG = '/opt/homebrew/bin/ffmpeg';
const FFPROBE = '/opt/homebrew/bin/ffprobe';
const VIDEO = process.argv[2] || path.join(homedir(), 'Desktop/eyes.mp4');
const N_FRAMES = 6;

function freePort(): Promise<number> {
  return new Promise((res, rej) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = (s.address() as AddressInfo).port;
      s.close(() => res(p));
    });
    s.on('error', rej);
  });
}

function probeDuration(file: string): number {
  const r = spawnSync(
    FFPROBE,
    ['-v', 'error', '-show_entries', 'format=duration', '-of', 'default=nw=1:nk=1', file],
    { encoding: 'utf8' },
  );
  const d = parseFloat((r.stdout || '').trim());
  return Number.isFinite(d) && d > 0 ? d : 0;
}

function extractFrames(file: string, outDir: string): string[] {
  const dur = probeDuration(file);
  const frames: string[] = [];
  for (let i = 0; i < N_FRAMES; i++) {
    // Campiona istanti equidistanti; per durata sconosciuta ripiega su 1 frame.
    const t = dur ? (dur * (i + 0.5)) / N_FRAMES : 0;
    const out = path.join(outDir, `f${i}.jpg`);
    const r = spawnSync(
      FFMPEG,
      [
        '-ss',
        t.toFixed(2),
        '-i',
        file,
        '-frames:v',
        '1',
        '-vf',
        'scale=448:448:force_original_aspect_ratio=decrease',
        '-q:v',
        '4',
        '-y',
        out,
      ],
      { stdio: 'ignore' },
    );
    if (r.status === 0 && existsSync(out)) frames.push(out);
    if (!dur) break;
  }
  return frames;
}

async function waitHealth(port: number, ms = 120000): Promise<boolean> {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/health`);
      if (r.ok) return true;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error('llama-server non pronto entro il timeout');
}

async function main(): Promise<void> {
  for (const [label, p] of [
    ['binario', BIN],
    ['modello', MODEL],
    ['mmproj', MMPROJ],
    ['video', VIDEO],
  ]) {
    if (!existsSync(p)) throw new Error(`${label} mancante: ${p}`);
  }

  const port = await freePort();
  console.log(`▶ avvio llama-server su :${port} ...`);
  const t0 = Date.now();
  const srv = spawn(
    BIN,
    [
      '--model',
      MODEL,
      '--mmproj',
      MMPROJ,
      '--host',
      '127.0.0.1',
      '--port',
      String(port),
      '-ngl',
      '99',
      '-c',
      '8192',
      '--no-warmup',
    ],
    { stdio: ['ignore', 'ignore', 'inherit'] },
  );

  // L'evento 'error' del ChildProcess (es. ENOEXEC/EACCES/ENOMEM allo spawn)
  // è asincrono: senza listener verrebbe ri-lanciato come eccezione non catturata,
  // bypassando il try/catch e il '✗' pulito. Lo trasformiamo in una promise che
  // rigetta, da mettere in race con l'attesa di readiness.
  const srvError = new Promise<never>((_, rej) => {
    srv.on('error', (e) => rej(new Error(`avvio llama-server fallito: ${e.message}`)));
  });

  let tmp: string | undefined;
  try {
    await Promise.race([waitHealth(port), srvError]);
    console.log(`✓ server pronto in ${((Date.now() - t0) / 1000).toFixed(1)}s`);

    tmp = mkdtempSync(path.join(tmpdir(), 'vlm-spike-'));
    const frames = extractFrames(VIDEO, tmp);
    console.log(`✓ estratti ${frames.length} frame da ${path.basename(VIDEO)}`);
    if (!frames.length) throw new Error('nessun frame estratto');

    const images = frames.map((f) => ({
      type: 'image_url',
      image_url: { url: `data:image/jpeg;base64,${readFileSync(f).toString('base64')}` },
    }));

    const body = {
      messages: [
        {
          role: 'system',
          content: 'Sei un assistente che cataloga video. Rispondi SEMPRE in italiano.',
        },
        {
          role: 'user',
          content: [
            {
              type: 'text',
              text: 'Questi sono fotogrammi estratti in ordine cronologico da un breve video. Descrivi cosa succede nel video e fornisci dei tag tematici per catalogarlo.',
            },
            ...images,
          ],
        },
      ],
      response_format: {
        type: 'json_schema',
        json_schema: {
          name: 'video_catalog',
          strict: true,
          schema: {
            type: 'object',
            additionalProperties: false,
            properties: {
              description: {
                type: 'string',
                description: 'Descrizione in italiano di cosa accade nel video',
              },
              tags: {
                type: 'array',
                items: { type: 'string' },
                description: 'Tag tematici in italiano',
              },
            },
            required: ['description', 'tags'],
          },
        },
      },
      temperature: 0.2,
      max_tokens: 512,
    };

    console.log('▶ inferenza ...');
    const tInf = Date.now();
    const res = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    // Leggi il body grezzo PRIMA di controllare res.ok: se il server risponde
    // con un errore non-JSON (proxy 502, body vuoto, HTML), res.json() lancerebbe
    // un errore di parsing fuorviante mascherando il vero status HTTP.
    const text = await res.text();
    if (!res.ok) throw new Error(`HTTP ${res.status}: ${text}`);
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      throw new Error(`risposta non-JSON dal server: ${text}`);
    }
    const content =
      (json as { choices?: { message?: { content?: string } }[] }).choices?.[0]?.message?.content ??
      '';
    console.log(`✓ inferenza in ${((Date.now() - tInf) / 1000).toFixed(1)}s\n`);
    console.log('─── RAW ───\n' + content + '\n');
    try {
      const parsed = JSON.parse(content) as { description?: unknown; tags?: unknown };
      console.log('─── PARSED ───');
      console.log('description:', parsed.description);
      console.log('tags:', parsed.tags);
    } catch (e) {
      console.log('⚠ output non è JSON valido:', (e as Error).message);
    }
  } finally {
    // Pulizia della temp dir nel finally: se fetch/parse lanciano, i frame JPEG
    // base64 non restano su disco in tmpdir.
    if (tmp) rmSync(tmp, { recursive: true, force: true });
    srv.kill('SIGTERM');
  }
}

main().catch((e) => {
  console.error('✗', (e as Error).message);
  process.exit(1);
});
