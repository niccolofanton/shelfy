// ─────────────────────────────────────────────────────────────────────────────
// Relay del feedback di SHELFY.
//
// L'app (electron/feedback.js, ramo RELAY_URL) POSTa qui un payload JSON
// { message, version, attachments } e questo Worker inoltra l'email via Resend
// tenendo la chiave (env.RESEND_API_KEY) come SECRET server-side. La chiave NON
// viaggia mai nel bundle dell'app né in git: l'app conosce solo l'URL pubblico
// di questo Worker, che NON è un segreto.
//
// Il formato della risposta combacia con quanto l'app si aspetta dal relay:
//   200 → { id }            (l'app lo mappa in { ok: true, id })
//   !2xx → { error|message } (l'app lo mappa in { ok: false, error })
//
// Difese (endpoint pubblico, quindi il payload è UNTRUSTED):
//   1. rate-limit per IP (binding nativa) contro i burst da un singolo client;
//   2. budget giornaliero GLOBALE in KV per non bruciare la quota Resend;
//   3. validazione + cap su messaggio e allegati, allineati a electron/feedback.js.
// ─────────────────────────────────────────────────────────────────────────────

// Cap allineati a electron/feedback.js: l'app è untrusted anche dal punto di
// vista del Worker, quindi NON ci fidiamo dei limiti che applica il client.
const MAX_MESSAGE_LEN = 5000;
const MAX_ATTACHMENTS = 5;
const MAX_ATTACHMENT_B64 = 7 * 1024 * 1024; // ~5 MB binari per file
const MAX_TOTAL_B64 = 16 * 1024 * 1024; // ~12 MB binari complessivi
const BASE64_RE = /^[A-Za-z0-9+/]+={0,2}$/;

// Guardia di taglia sul corpo della richiesta, oltre i cap per-allegato: rifiuta
// payload assurdi prima ancora di leggerli in memoria.
const MAX_BODY_BYTES = 24 * 1024 * 1024;

const json = (obj, status = 200) =>
  new Response(JSON.stringify(obj), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });

// Normalizza gli allegati nel formato atteso da Resend ({ filename, content }),
// scartando i malformati / non-base64 / troppo grandi (per-file) o che
// sforerebbero il cap aggregato. Identico in spirito a feedback.js.
function sanitizeAttachments(attachments) {
  if (!Array.isArray(attachments)) return [];
  const out = [];
  let total = 0;
  for (const a of attachments) {
    if (out.length >= MAX_ATTACHMENTS) break;
    if (!a || typeof a.content !== 'string' || !a.content) continue;
    if (a.content.length > MAX_ATTACHMENT_B64) continue;
    if (!BASE64_RE.test(a.content)) continue;
    if (total + a.content.length > MAX_TOTAL_B64) break;
    total += a.content.length;
    out.push({
      filename: String(a.filename || `screenshot-${out.length + 1}.png`).slice(0, 120),
      content: a.content,
    });
  }
  return out;
}

export default {
  async fetch(request, env) {
    // Accettiamo solo POST: niente CORS (l'app chiama dal main process, non dal
    // browser) e niente metodi inattesi.
    if (request.method !== 'POST') return json({ error: 'Method not allowed' }, 405);

    const contentLength = Number(request.headers.get('content-length') || 0);
    if (contentLength > MAX_BODY_BYTES) return json({ error: 'Payload too large' }, 413);

    // ── 1) Rate-limit per IP (anti-burst) ───────────────────────────────────
    const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
    if (env.PER_IP) {
      const { success } = await env.PER_IP.limit({ key: ip });
      if (!success) return json({ error: 'Troppi invii ravvicinati, riprova tra poco.' }, 429);
    }

    // ── 2) Parse + validazione del payload ──────────────────────────────────
    let body;
    try {
      body = await request.json();
    } catch {
      return json({ error: 'Invalid JSON' }, 400);
    }
    const text = (typeof body?.message === 'string' ? body.message : '').trim();
    const version = typeof body?.version === 'string' ? body.version.slice(0, 64) : '';
    const files = sanitizeAttachments(body?.attachments);

    if (!text && files.length === 0) return json({ error: 'Messaggio vuoto.' }, 400);
    if (text.length > MAX_MESSAGE_LEN) return json({ error: 'Messaggio troppo lungo.' }, 400);

    // ── 3) Budget giornaliero GLOBALE (protegge la quota Resend) ─────────────
    // Contatore per chiave-giorno in KV. Read-modify-write NON atomico (KV è
    // eventually consistent) → è un cap "morbido": sotto flood può lasciar
    // passare qualche email in più, ma tiene il volume nell'ordine di grandezza
    // giusto rispetto alla quota Resend. Per noi è un guard-rail, non una
    // contabilità esatta.
    const cap = Number(env.DAILY_CAP || 90);
    let dayKey = null;
    if (env.BUDGET) {
      // Chiave-giorno in UTC. (Nota: in un Worker l'orologio è "congelato"
      // durante la richiesta, ma per una chiave a granularità giornaliera è
      // perfettamente adeguato.)
      const day = new Date().toISOString().slice(0, 10); // YYYY-MM-DD
      dayKey = `sent:${day}`;
      const used = Number((await env.BUDGET.get(dayKey)) || 0);
      if (used >= cap) {
        return json({ error: 'Limite giornaliero di feedback raggiunto, riprova domani.' }, 429);
      }
    }

    // ── 4) Inoltro a Resend ──────────────────────────────────────────────────
    if (!env.RESEND_API_KEY)
      return json({ error: 'Relay non configurato (manca il secret).' }, 500);
    if (!env.TO) return json({ error: 'Relay non configurato (manca TO).' }, 500);

    const bodyLines = [
      text || '(nessun messaggio)',
      '',
      '—',
      files.length ? `Allegati: ${files.length} screenshot` : null,
      version ? `Versione SHELFY: ${version}` : null,
    ].filter((l) => l !== null);

    const payload = {
      from: env.FROM || 'SHELFY <onboarding@resend.dev>',
      to: env.TO,
      subject: 'Feedback — SHELFY',
      text: bodyLines.join('\n'),
    };
    if (files.length) payload.attachments = files;

    let res;
    try {
      res = await fetch('https://api.resend.com/emails', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${env.RESEND_API_KEY}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(payload),
      });
    } catch (err) {
      return json({ error: err?.message || 'Errore di rete verso Resend.' }, 502);
    }

    if (!res.ok) {
      let detail = `HTTP ${res.status}`;
      try {
        const j = await res.json();
        if (j?.message) detail = j.message;
      } catch {
        /* corpo non-JSON: tieni lo status */
      }
      return json({ error: detail }, 502);
    }
    const out = await res.json().catch(() => ({}));

    // ── 5) Incrementa il budget giornaliero (best-effort) ────────────────────
    // TTL 48h: la chiave-giorno scade da sola, niente pulizia manuale.
    if (env.BUDGET && dayKey) {
      const used = Number((await env.BUDGET.get(dayKey)) || 0);
      await env.BUDGET.put(dayKey, String(used + 1), { expirationTtl: 60 * 60 * 48 });
    }

    return json({ id: out?.id });
  },
};
