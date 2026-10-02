// UI strings for signing in to the web app (web/src/auth): the sign-in page
// ("email me a link", or the operator's link when email is off) and the page a
// sign-in link opens. The desktop app has no sign-in.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Accedi a Shelfy',
    loading: 'Caricamento…',
    emailLabel: 'Email',
    emailPlaceholder: 'tu@esempio.com',
    sendLink: 'Inviami un link di accesso',
    sending: 'Invio…',
    linkSent:
      'Se questo indirizzo ha un account, è in arrivo un link di accesso. Vale 15 minuti e funziona una volta sola.',
    sendAnother: 'Invia un altro link',
    askOperator:
      "L'accesso via email non è attivo su questo server. Chiedi all'amministratore un link di accesso.",
    invalidLink: 'Questo link di accesso non è valido, è scaduto o è già stato usato.',
    invalidEmail: 'Inserisci un indirizzo email valido.',
    rateLimited: 'Troppe richieste. Riprova tra qualche minuto.',
    unreachable: 'Il server non risponde. Riprova tra poco.',
    genericError: 'Qualcosa non ha funzionato. Riprova.',
    magicTitle: 'Completa l’accesso',
    magicHint: 'Premi il pulsante per accedere a Shelfy su questo dispositivo.',
    signIn: 'Accedi',
    signingIn: 'Accesso…',
    backToSignIn: "Torna all'accesso",
  },
  en: {
    title: 'Sign in to Shelfy',
    loading: 'Loading…',
    emailLabel: 'Email',
    emailPlaceholder: 'you@example.com',
    sendLink: 'Email me a sign-in link',
    sending: 'Sending…',
    linkSent:
      'If this address has an account, a sign-in link is on its way. It is valid for 15 minutes and works once.',
    sendAnother: 'Send another link',
    askOperator: 'Email sign-in is off on this server. Ask the operator for a sign-in link.',
    invalidLink: 'This sign-in link is invalid, has expired or was already used.',
    invalidEmail: 'Enter a valid email address.',
    rateLimited: 'Too many requests. Try again in a few minutes.',
    unreachable: 'The server is not answering. Try again shortly.',
    genericError: 'Something went wrong. Try again.',
    magicTitle: 'Finish signing in',
    magicHint: 'Press the button to sign in to Shelfy on this device.',
    signIn: 'Sign in',
    signingIn: 'Signing in…',
    backToSignIn: 'Back to sign in',
  },
} satisfies LangMessages;
