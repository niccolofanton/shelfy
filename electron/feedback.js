'use strict';

// ─────────────────────────────────────────────────────────────────────────────
// Feedback → email. In produzione l'app POSTa a un relay (Cloudflare Worker) che
// custodisce la chiave Resend come SECRET server-side: la chiave NON tocca mai il
// renderer né il bundle. L'invio avviene QUI nel main process (niente CORS), con
// `fetch` globale (Node 20 / Electron 31).
//
// Il Worker relay vive in workers/feedback/ — vedi il suo README per deploy e
// rotazione della chiave (`wrangler secret put RESEND_API_KEY`, senza toccare l'app).
//
// Configurazione (in ordine di preferenza):
//   1. RELAY (default) → SHELFY_FEEDBACK_RELAY_URL, con fallback all'URL pubblico
//      del Worker qui sotto (l'URL NON è un segreto). L'app POSTa lì il payload e
//      non vede mai la chiave; il Worker fa da rate-limit e validazione.
//   2. RESEND diretto → SOLO sviluppo, se disattivi il relay (RELAY_URL vuoto):
//      imposta SHELFY_RESEND_API_KEY via env (mai committata). Nessuna chiave è
//      più hard-coded qui, quindi non viaggia nel bundle.
// ─────────────────────────────────────────────────────────────────────────────

// Endpoint relay che custodisce la chiave server-side (default di produzione).
// L'URL pubblico del Worker non è un segreto: può stare in chiaro e in git.
// Override via env per puntare a staging / `wrangler dev`; stringa vuota per
// disattivare il relay e usare l'invio diretto di sviluppo (vedi sotto).
const RELAY_URL = (
  process.env.SHELFY_FEEDBACK_RELAY_URL || 'https://shelfy-feedback.niccolofanton1997.workers.dev'
).trim();

// API key Resend per l'invio DIRETTO: usato solo in sviluppo quando il relay è
// disattivato (RELAY_URL vuoto). Nessun default hard-coded — in produzione la
// chiave vive solo come secret del Worker relay.
const RESEND_API_KEY = (process.env.SHELFY_RESEND_API_KEY || '').trim();

// Mittente. `onboarding@resend.dev` funziona senza dominio verificato (ma solo
// verso l'email del proprietario dell'account). Con un dominio verificato puoi
// usare es. 'SHELFY <feedback@tuodominio.com>'.
const FROM = 'SHELFY <onboarding@resend.dev>';

// Destinatario fisso del feedback (lo sviluppatore).
const TO = 'niccolofanton1997@gmail.com';

// Allegati: solo immagini, max 5 file. Cap sul base64 per non sforare i limiti
// di dimensione dell'email su Resend (~40MB totali, restiamo molto sotto).
const MAX_ATTACHMENTS = 5;
const MAX_ATTACHMENT_B64 = 7 * 1024 * 1024; // ~5 MB binari per file
// Cap aggregato su tutti gli allegati: il renderer è UNTRUSTED dal punto di
// vista del main process, quindi non ci fidiamo dei suoi limiti per-file.
const MAX_TOTAL_B64 = 16 * 1024 * 1024; // ~12 MB binari complessivi
// Charset base64 (con eventuale padding finale): scarta byte arbitrari/non-immagine.
const BASE64_RE = /^[A-Za-z0-9+/]+={0,2}$/;

// Timeout sulle chiamate HTTP in uscita (relay / Resend): un endpoint che apre la
// connessione ma non risponde mai (rete bloccata, captive portal, host appeso)
// terrebbe l'await sospeso fino al timeout del socket OS (anche minuti), lasciando
// il renderer fermo sullo spinner. Con l'AbortController la fetch viene interrotta
// e il catch esistente la mappa in { ok:false, error } → l'utente può riprovare.
const FETCH_TIMEOUT_MS = 20_000;

/**
 * Normalizza gli allegati ricevuti dal renderer nel formato atteso da Resend
 * ({ filename, content }), scartando quelli malformati, non-base64, troppo
 * grandi (per-file) o che sforerebbero il cap aggregato.
 */
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

/**
 * Invia un messaggio di feedback via Resend.
 * @param {{ message?: string, version?: string, attachments?: Array<{ filename?: string, content?: string }> }} payload
 * @returns {Promise<{ ok: boolean, id?: string, error?: string }>}
 */
async function sendFeedback({ message, version, attachments } = {}) {
  const text = (message || '').trim();
  const files = sanitizeAttachments(attachments);
  if (!text && files.length === 0) return { ok: false, error: 'Messaggio vuoto.' };
  if (text.length > 5000) return { ok: false, error: 'Messaggio troppo lungo.' };

  // Relay (consigliato): inoltra il payload a un endpoint che custodisce la
  // chiave server-side. L'app non vede mai il segreto.
  if (RELAY_URL) {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), FETCH_TIMEOUT_MS);
    try {
      const res = await fetch(RELAY_URL, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message: text, version, attachments: files }),
        signal: ac.signal,
      });
      if (!res.ok) {
        let detail = `HTTP ${res.status}`;
        try {
          const j = await res.json();
          if (j?.message || j?.error) detail = j.message || j.error;
        } catch {
          /* corpo non-JSON: tieni lo status */
        }
        console.error('[feedback] relay error:', detail);
        return { ok: false, error: detail };
      }
      const json = await res.json().catch(() => ({}));
      return { ok: true, id: json?.id };
    } catch (err) {
      console.error('[feedback] relay send failed:', err);
      return { ok: false, error: err?.message || 'Errore di rete.' };
    } finally {
      clearTimeout(timer);
    }
  }

  if (!RESEND_API_KEY) {
    return {
      ok: false,
      error: 'Invio non configurato (imposta SHELFY_FEEDBACK_RELAY_URL o SHELFY_RESEND_API_KEY).',
    };
  }

  const bodyLines = [
    text || '(nessun messaggio)',
    '',
    '—',
    files.length ? `Allegati: ${files.length} screenshot` : null,
    version ? `Versione SHELFY: ${version}` : null,
  ].filter((l) => l !== null);

  const payload = {
    from: FROM,
    to: TO,
    subject: 'Feedback — SHELFY',
    text: bodyLines.join('\n'),
  };
  if (files.length) payload.attachments = files;

  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), FETCH_TIMEOUT_MS);
  try {
    const res = await fetch('https://api.resend.com/emails', {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${RESEND_API_KEY}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
      signal: ac.signal,
    });

    if (!res.ok) {
      let detail = `HTTP ${res.status}`;
      try {
        const j = await res.json();
        if (j?.message) detail = j.message;
      } catch {
        /* corpo non-JSON: tieni lo status */
      }
      console.error('[feedback] Resend error:', detail);
      return { ok: false, error: detail };
    }

    const json = await res.json().catch(() => ({}));
    return { ok: true, id: json?.id };
  } catch (err) {
    console.error('[feedback] send failed:', err);
    return { ok: false, error: err?.message || 'Errore di rete.' };
  } finally {
    clearTimeout(timer);
  }
}

module.exports = { sendFeedback };
