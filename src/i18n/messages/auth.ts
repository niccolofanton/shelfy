// UI strings for signing in to the web app (web/src/auth): the sign-in page
// (a passkey, "email me a link", or the operator's link when email is off), the
// page a sign-in link opens, the re-authentication dialog and its link page,
// and the device-approval page (`/device`). `passkey<Reason>` explain a passkey
// ceremony that failed in the browser (src/hooks/useFailureText.ts), in the
// account's Settings too. The desktop app has no sign-in.
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

    // Passkeys
    passkeySignIn: 'Accedi con una passkey',
    passkeySigningIn: 'In attesa della passkey…',
    passkeyFirstTime:
      'Prima volta qui? Accedi con un link, poi aggiungi una passkey in Impostazioni → Account.',
    or: 'oppure',
    passkeyNotRegistered:
      'Questa passkey non è registrata qui: forse è stata rimossa. Accedi con un link.',
    passkeyCancelled: 'Nessuna passkey usata: la richiesta è stata annullata o è scaduta.',
    passkeyExists: 'Questo dispositivo ha già una passkey per il tuo account.',
    passkeyUnsupported: 'Questo browser o dispositivo non supporta le passkey.',
    passkeyOrigin:
      'Le passkey funzionano solo all’indirizzo del server: apri Shelfy dal suo indirizzo.',
    passkeyFailed: 'La passkey non ha funzionato. Riprova.',

    // Re-authentication dialog
    reauthTitle: 'Conferma che sei tu',
    reauthBody:
      'Questa azione richiede un accesso recente, degli ultimi 5 minuti. Conferma con una passkey o con un link.',
    reauthAgain:
      'Non risulta ancora confermato. Apri il link in questo browser, o usa una passkey.',
    reauthPasskey: 'Usa una passkey',
    reauthPasskeyBusy: 'In attesa della passkey…',
    reauthEmail: 'Inviami un link via email',
    reauthEmailSent:
      'Il link è in arrivo all’indirizzo del tuo account e vale 15 minuti. Aprilo in questo browser: questa finestra prosegue da sola.',
    reauthOperatorTitle: 'Nessuna passkey né email?',
    reauthOperator: 'Sul server esegui questo comando e apri in questo browser il link che stampa.',
    reauthContinue: 'Ho aperto il link',
    reauthNoPasskey: 'Questo account non ha ancora una passkey: conferma con un link.',

    // Re-authentication link page (/login/reauth)
    reauthLinkHint:
      'Premi il pulsante per confermare, in questo browser, l’azione che hai avviato in Shelfy.',
    reauthConfirm: 'Conferma',
    reauthConfirming: 'Conferma…',
    reauthLinkDone:
      'Confermato. Torna alla scheda in cui hai iniziato: prosegue da sola. La conferma vale 5 minuti.',
    reauthSignedOut:
      'Questo browser non ha effettuato l’accesso a Shelfy. Apri il link nel browser in cui hai iniziato.',
    invalidReauthLink: 'Questo link di conferma non è valido, è scaduto o è già stato usato.',
    backToShelfy: 'Torna a Shelfy',

    // Device approval (/device)
    deviceTitle: 'Approva un dispositivo',
    deviceIntro:
      'Lo strumento di migrazione (shelfy-migrate login) mostra un codice. Inseriscilo qui per permettere a quel dispositivo di usare il tuo account per 7 giorni.',
    deviceWarning:
      'Approva solo un codice che vedi nel tuo terminale. Se qualcuno ti manda un codice da approvare, sta cercando di entrare nella tua libreria.',
    deviceCodeLabel: 'Codice',
    deviceApprove: 'Approva',
    deviceApproving: 'Approvazione…',
    deviceApproved: 'Approvato. Torna al terminale: lo strumento prosegue da solo.',
    deviceInvalid:
      'Questo codice non è valido, è scaduto o è già stato usato. Avvia di nuovo l’accesso nel terminale.',
    deviceReauth: 'Per approvare un dispositivo, conferma prima che sei tu.',
    deviceConfirm: 'Conferma che sei tu',
    deviceConfirming: 'Conferma in corso…',
    deviceRateLimited: 'Troppi tentativi. Riprova tra qualche minuto.',
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

    // Passkeys
    passkeySignIn: 'Sign in with a passkey',
    passkeySigningIn: 'Waiting for your passkey…',
    passkeyFirstTime:
      'First time here? Sign in with a link, then add a passkey in Settings → Account.',
    or: 'or',
    passkeyNotRegistered:
      'This passkey is not registered here: it may have been removed. Sign in with a link.',
    passkeyCancelled: 'No passkey was used: the request was cancelled or timed out.',
    passkeyExists: 'This device already holds a passkey for your account.',
    passkeyUnsupported: 'This browser or device does not support passkeys.',
    passkeyOrigin: 'Passkeys work only on the server’s address: open Shelfy from its own address.',
    passkeyFailed: 'The passkey did not work. Try again.',

    // Re-authentication dialog
    reauthTitle: 'Confirm it’s you',
    reauthBody:
      'This action needs a recent sign-in, from the last 5 minutes. Confirm with a passkey or a link.',
    reauthAgain: 'Not confirmed yet. Open the link in this browser, or use a passkey.',
    reauthPasskey: 'Use a passkey',
    reauthPasskeyBusy: 'Waiting for your passkey…',
    reauthEmail: 'Email me a link',
    reauthEmailSent:
      'A link is on its way to your account’s address; it is valid for 15 minutes. Open it in this browser: this window carries on by itself.',
    reauthOperatorTitle: 'No passkey or email?',
    reauthOperator: 'On the server, run this command and open the link it prints in this browser.',
    reauthContinue: 'I opened the link',
    reauthNoPasskey: 'This account has no passkey yet: confirm with a link.',

    // Re-authentication link page (/login/reauth)
    reauthLinkHint:
      'Press the button to confirm, in this browser, the action you started in Shelfy.',
    reauthConfirm: 'Confirm',
    reauthConfirming: 'Confirming…',
    reauthLinkDone:
      'Confirmed. Go back to the tab where you started: it carries on by itself. The confirmation lasts 5 minutes.',
    reauthSignedOut:
      'This browser is not signed in to Shelfy. Open the link in the browser where you started.',
    invalidReauthLink: 'This confirmation link is invalid, has expired or was already used.',
    backToShelfy: 'Back to Shelfy',

    // Device approval (/device)
    deviceTitle: 'Approve a device',
    deviceIntro:
      'The migration tool (shelfy-migrate login) shows a code. Enter it here to let that device use your account for 7 days.',
    deviceWarning:
      'Approve only a code your own terminal shows. If someone sends you a code to approve, they are trying to get into your library.',
    deviceCodeLabel: 'Code',
    deviceApprove: 'Approve',
    deviceApproving: 'Approving…',
    deviceApproved: 'Approved. Go back to your terminal: the tool carries on by itself.',
    deviceInvalid:
      'This code is invalid, has expired or was already used. Start the sign-in again in your terminal.',
    deviceReauth: 'To approve a device, confirm it’s you first.',
    deviceConfirm: 'Confirm it’s you',
    deviceConfirming: 'Confirming…',
    deviceRateLimited: 'Too many tries. Try again in a few minutes.',
  },
} satisfies LangMessages;
