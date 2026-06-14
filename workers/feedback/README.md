# shelfy-feedback — relay del feedback

Cloudflare Worker che riceve il feedback dall'app SHELFY e lo inoltra via Resend,
**tenendo la chiave Resend come secret server-side**. L'app conosce solo l'URL
pubblico di questo Worker (non un segreto); la chiave non viaggia mai nel bundle
né in git.

```
App (electron/feedback.js, ramo RELAY_URL)
  │  POST { message, version, attachments }
  ▼
Worker shelfy-feedback                     ← RESEND_API_KEY (secret)
  │  rate-limit per IP + budget giornaliero + validazione
  ▼
Resend  →  email a TO
```

## Setup (una tantum)

Dalla cartella `workers/feedback/`:

```bash
# 1) Crea il namespace KV per il budget giornaliero e copia l'id stampato
#    dentro wrangler.jsonc (campo kv_namespaces[0].id, al posto di PLACEHOLDER_KV_ID).
npx wrangler@latest kv namespace create FEEDBACK_BUDGET

# 2) Imposta la chiave Resend come secret (prompt interattivo: incolla la chiave).
npx wrangler@latest secret put RESEND_API_KEY

# 3) Deploy.
npx wrangler@latest deploy
```

Il deploy stampa l'URL pubblico (`https://shelfy-feedback.<account>.workers.dev`):
va messo come default di `RELAY_URL` in `electron/feedback.js`.

## Config

| Dove | Chiave | Cos'è |
| --- | --- | --- |
| `wrangler.jsonc` → `vars` | `FROM` | mittente (`onboarding@resend.dev` senza dominio verificato) |
| `wrangler.jsonc` → `vars` | `TO` | destinatario del feedback |
| `wrangler.jsonc` → `vars` | `DAILY_CAP` | tetto giornaliero globale di email (default 90, sotto i 100/giorno del free Resend) |
| secret | `RESEND_API_KEY` | chiave Resend, **mai** in chiaro nel repo |

> Con `onboarding@resend.dev` Resend recapita solo all'email del proprietario
> dell'account. Per inviare a un `TO` diverso o usare un mittente personalizzato,
> verifica un dominio su Resend e aggiorna `FROM`.

## Difese

1. **Rate-limit per IP** (binding nativa `PER_IP`): 3 invii / 60s per IP — anti-burst.
2. **Budget giornaliero globale** (KV `BUDGET`): tetto `DAILY_CAP` email/giorno per
   non bruciare la quota Resend anche sotto abuso distribuito. Cap "morbido" (KV è
   eventually consistent).
3. **Validazione**: solo `POST`, messaggio ≤ 5000 char, max 5 allegati immagine,
   base64 verificato, cap aggregato 16 MB — allineati a `electron/feedback.js`.

## Comandi utili

```bash
npx wrangler@latest tail shelfy-feedback         # log live
npx wrangler@latest secret list                  # secret impostati
npx wrangler@latest kv key get --binding BUDGET "sent:$(date -u +%F)"  # invii oggi
```
