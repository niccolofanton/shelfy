// Generic, surface-agnostic error strings used by data hooks whose failures are
// shown to the user (gallery load, etc.), by the error boundaries
// (src/components/ErrorBoundary.tsx) and by the pages an address has nothing
// for. Feature-specific errors live in their own namespace.
//
// `code.<code>`: one message per problem `code` of the web API (plan §2.9: the
// server sends codes, never UI prose), plus `network` when no answer came back.
// src/api/errors.ts picks them; web/tests/errors.test.ts checks that every
// code of crates/server/openapi.json has one in every language.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    loadPosts: 'Caricamento dei post non riuscito.',
    crashedTitle: 'Qualcosa è andato storto',
    crashed: "Questa sezione si è interrotta per un errore. Riprova, o ricarica l'app.",
    retry: 'Riprova',
    reload: 'Ricarica',
    close: 'Chiudi',
    notFoundTitle: 'Pagina non trovata',
    notFound: "A questo indirizzo non c'è niente.",
    postMissingTitle: 'Post non disponibile',
    postMissing: 'Questo post non è nella tua libreria, o è stato eliminato.',
    unavailableTitle: 'Non ancora disponibile',
    unavailable: 'Questa sezione non è ancora disponibile qui.',
    backToLibrary: 'Torna alla libreria',
    'code.bad_request': 'La richiesta non è valida.',
    'code.invalid_cursor': "L'elenco è cambiato durante il caricamento. Ricaricalo.",
    'code.invalid_link': 'Questo link non è valido, è scaduto o è già stato usato.',
    'code.challenge_expired': 'La richiesta della passkey è scaduta. Riprova.',
    'code.passkey_invalid': 'Questa passkey non è stata accettata.',
    'code.invalid_device_code': 'Questo codice non è valido, è scaduto o è già stato usato.',
    'code.unauthorized': 'La sessione è scaduta. Accedi di nuovo.',
    'code.forbidden': 'Non hai il permesso di farlo.',
    'code.csrf_failed': 'Richiesta bloccata per sicurezza. Ricarica la pagina e riprova.',
    'code.reauth_required': 'Conferma la tua identità per continuare.',
    'code.quota_exceeded': 'Lo spazio di archiviazione è esaurito.',
    'code.not_found': 'Non esiste più.',
    'code.method_not_allowed': 'Il server non supporta questa azione.',
    'code.conflict': 'Questa modifica è in conflitto con un’altra. Ricarica e riprova.',
    'code.payload_too_large': 'È troppo grande per essere inviato.',
    'code.unsupported_media_type': 'Questo tipo di file non è supportato.',
    'code.validation_failed': 'Alcuni valori non sono validi.',
    'code.provider_key_invalid': 'Il provider AI ha rifiutato la chiave.',
    'code.capture_blocked': 'Non è stato possibile catturare il sito.',
    'code.not_available': 'Questa azione non è ancora disponibile.',
    'code.user_locked': 'La tua libreria è in manutenzione. Riprova tra qualche minuto.',
    'code.extension_outdated': "L'estensione del browser è obsoleta: aggiornala.",
    'code.rate_limited': 'Troppe richieste. Riprova tra poco.',
    'code.internal': 'Si è verificato un errore sul server.',
    'code.unavailable': 'Il server è occupato. Riprova tra poco.',
    'code.timeout': 'Il server ha impiegato troppo a rispondere.',
    'code.network': 'Il server non risponde. Controlla la connessione.',
  },
  en: {
    loadPosts: 'Could not load posts.',
    crashedTitle: 'Something went wrong',
    crashed: 'This section stopped because of an error. Try again, or reload the app.',
    retry: 'Try again',
    reload: 'Reload',
    close: 'Close',
    notFoundTitle: 'Page not found',
    notFound: 'There is nothing at this address.',
    postMissingTitle: 'Post not available',
    postMissing: 'This post is not in your library, or it was deleted.',
    unavailableTitle: 'Not available yet',
    unavailable: 'This section is not available here yet.',
    backToLibrary: 'Back to the library',
    'code.bad_request': 'The request was not valid.',
    'code.invalid_cursor': 'The list changed while it was loading. Reload it.',
    'code.invalid_link': 'This link is invalid, has expired or was already used.',
    'code.challenge_expired': 'The passkey request expired. Try again.',
    'code.passkey_invalid': 'This passkey was not accepted.',
    'code.invalid_device_code': 'This code is invalid, has expired or was already used.',
    'code.unauthorized': 'Your session has ended. Sign in again.',
    'code.forbidden': 'You are not allowed to do this.',
    'code.csrf_failed': 'The request was blocked for security. Reload the page and try again.',
    'code.reauth_required': 'Confirm it is you to continue.',
    'code.quota_exceeded': 'Your storage is full.',
    'code.not_found': 'It no longer exists.',
    'code.method_not_allowed': 'The server does not support this action.',
    'code.conflict': 'This clashes with another change. Reload and try again.',
    'code.payload_too_large': 'This is too large to send.',
    'code.unsupported_media_type': 'This file type is not supported.',
    'code.validation_failed': 'Some values are not valid.',
    'code.provider_key_invalid': 'The AI provider refused the key.',
    'code.capture_blocked': 'The site could not be captured.',
    'code.not_available': 'This action is not available yet.',
    'code.user_locked': 'Your library is under maintenance. Try again in a few minutes.',
    'code.extension_outdated': 'The browser extension is out of date: update it.',
    'code.rate_limited': 'Too many requests. Try again shortly.',
    'code.internal': 'Something went wrong on the server.',
    'code.unavailable': 'The server is busy. Try again shortly.',
    'code.timeout': 'The server took too long to answer.',
    'code.network': 'The server is not answering. Check the connection.',
  },
} satisfies LangMessages;
