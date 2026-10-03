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
    'code.invalid_pairing_code':
      "Il codice di abbinamento non è valido, è scaduto o è già stato usato. Abbina di nuovo l'estensione.",
    'code.unauthorized': 'La sessione è scaduta. Accedi di nuovo.',
    'code.forbidden': 'Non hai il permesso di farlo.',
    'code.csrf_failed': 'Richiesta bloccata per sicurezza. Ricarica la pagina e riprova.',
    'code.reauth_required': 'Conferma la tua identità per continuare.',
    'code.quota_exceeded': 'Lo spazio di archiviazione è esaurito.',
    'code.storage_full': 'Lo spazio del server è esaurito. Riprova più tardi.',
    'code.not_found': 'Non esiste più.',
    'code.sync_run_not_found': 'Questa sincronizzazione non esiste più.',
    'code.method_not_allowed': 'Il server non supporta questa azione.',
    'code.conflict': 'Questa modifica è in conflitto con un’altra. Ricarica e riprova.',
    'code.source_disabled': 'Questa modalità di acquisizione è disattivata per questa piattaforma.',
    'code.upload_consumed': 'Questo file caricato è già stato usato. Caricalo di nuovo.',
    'code.payload_too_large': 'È troppo grande per essere inviato.',
    'code.unsupported_media_type': 'Questo tipo di file non è supportato.',
    'code.confirm_token_invalid':
      'La conferma è scaduta o non corrisponde. Richiedi una nuova stima.',
    'code.validation_failed': 'Alcuni valori non sono validi.',
    'code.provider_key_invalid': 'Il provider AI ha rifiutato la chiave.',
    'code.import_checkpoint_unbound':
      'Questo import precedente non può essere ripreso in sicurezza. Avvia un nuovo import e carica di nuovo il file.',
    'code.import_format_unknown':
      'Il file di importazione non è valido o usa un formato non supportato.',
    'code.ai_not_configured': 'Nessun provider AI è configurato per questa operazione.',
    'code.ai_vault_disabled':
      'Le credenziali dei provider personali sono disabilitate. Contatta il proprietario del server.',
    'code.ai_consent_required': 'Serve il tuo consenso per inviare i contenuti a questo provider.',
    'code.provider_offline': 'Il provider AI non è raggiungibile ora. Riprova più tardi.',
    'code.provider_unavailable': 'Il provider AI non è disponibile al momento. Riprova tra poco.',
    'code.provider_quota_exhausted': 'Il provider AI non ha più credito o quota disponibile.',
    'code.capture_unavailable': 'Il servizio di cattura non è disponibile. Riprova più tardi.',
    'code.capture_daily_limit': 'Hai raggiunto il limite giornaliero di catture.',
    'code.capture_blocked': 'Non è stato possibile catturare il sito.',
    'code.unsupported_link': 'Questo link non può essere salvato.',
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
    'code.invalid_pairing_code':
      'The pairing code is invalid, has expired or was already used. Pair the extension again.',
    'code.unauthorized': 'Your session has ended. Sign in again.',
    'code.forbidden': 'You are not allowed to do this.',
    'code.csrf_failed': 'The request was blocked for security. Reload the page and try again.',
    'code.reauth_required': 'Confirm it is you to continue.',
    'code.quota_exceeded': 'Your storage is full.',
    'code.storage_full': 'The server is out of storage space. Try again later.',
    'code.not_found': 'It no longer exists.',
    'code.sync_run_not_found': 'This sync run no longer exists.',
    'code.method_not_allowed': 'The server does not support this action.',
    'code.conflict': 'This clashes with another change. Reload and try again.',
    'code.source_disabled': 'This capture mode is turned off for this platform.',
    'code.upload_consumed': 'This uploaded file was already used. Upload it again.',
    'code.payload_too_large': 'This is too large to send.',
    'code.unsupported_media_type': 'This file type is not supported.',
    'code.confirm_token_invalid':
      'The confirmation expired or does not match. Request a new estimate.',
    'code.validation_failed': 'Some values are not valid.',
    'code.provider_key_invalid': 'The AI provider refused the key.',
    'code.import_checkpoint_unbound':
      'This older import cannot be resumed safely. Start a new import and upload the file again.',
    'code.import_format_unknown': 'The import file is invalid or uses an unsupported format.',
    'code.ai_not_configured': 'No AI provider is set up for this task.',
    'code.ai_vault_disabled':
      'Credentials for personal providers are disabled. Contact the server owner.',
    'code.ai_consent_required': 'Sending your content to this provider needs your consent first.',
    'code.provider_offline': 'The AI provider cannot be reached right now. Try again later.',
    'code.provider_unavailable': 'The AI provider is unavailable right now. Try again shortly.',
    'code.provider_quota_exhausted': 'The AI provider has no credit or quota left.',
    'code.capture_unavailable': 'The capture service is unavailable. Try again later.',
    'code.capture_daily_limit': 'You have reached the daily capture limit.',
    'code.capture_blocked': 'The site could not be captured.',
    'code.unsupported_link': 'This link cannot be saved.',
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
