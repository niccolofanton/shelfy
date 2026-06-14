// Strings for the in-app "send feedback" modal (message + screenshots → email).
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Invia feedback',
    intro:
      "Hai trovato un bug, un'idea o qualcosa che non torna? Scrivi qui sotto: il messaggio verrà inviato via email direttamente a chi sviluppa SHELFY. Conferma l'invio e ci pensa l'app.",
    messageLabel: 'Messaggio',
    messagePlaceholder: "Descrivi il problema o l'idea il più chiaramente possibile…",
    screenshotLabel: 'Screenshot',
    optional: '(opzionale)',
    attach: 'Allega',
    attachHint: "Allega un'immagine o incollala (Cmd/Ctrl+V) nel messaggio.",
    fileTooBig: '"{name}" supera 5 MB e non è stato allegato.',
    unnamedImage: 'immagine',
    tooManyFiles: 'Puoi allegare al massimo {max} immagini.',
    attachTooHeavy: 'Allegati troppo pesanti: alcuni file non sono stati allegati.',
    sendFailed: 'Invio non riuscito.',
    send: 'Invia',
    confirmSend: 'Inviare il feedback al team di SHELFY?',
    confirmSendDetail: "Il messaggio parte subito, direttamente dall'app.",
    confirmClose: 'Hai un messaggio non inviato. Vuoi chiudere e perderlo?',
    yesSend: 'Sì, invia',
    closeWithoutSending: 'Chiudi senza inviare',
    keepWriting: 'Continua a scrivere',
    sent: 'Feedback inviato. Grazie!',
  },
  en: {
    title: 'Send feedback',
    intro:
      'Found a bug, have an idea, or something not working right? Write it below: the message will be emailed straight to the developer of SHELFY. Confirm sending and the app takes care of the rest.',
    messageLabel: 'Message',
    messagePlaceholder: 'Describe the problem or idea as clearly as possible…',
    screenshotLabel: 'Screenshot',
    optional: '(optional)',
    attach: 'Attach',
    attachHint: 'Attach an image or paste it (Cmd/Ctrl+V) into the message.',
    fileTooBig: '"{name}" exceeds 5 MB and was not attached.',
    unnamedImage: 'image',
    tooManyFiles: 'You can attach at most {max} images.',
    attachTooHeavy: 'Attachments too large: some files were not attached.',
    sendFailed: 'Sending failed.',
    send: 'Send',
    confirmSend: 'Send the feedback to the SHELFY team?',
    confirmSendDetail: 'The message goes out right away, straight from the app.',
    confirmClose: 'You have an unsent message. Close and lose it?',
    yesSend: 'Yes, send',
    closeWithoutSending: 'Close without sending',
    keepWriting: 'Keep writing',
    sent: 'Feedback sent. Thank you!',
  },
} satisfies LangMessages;
