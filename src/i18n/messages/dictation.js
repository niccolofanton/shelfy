// UI strings surfaced by the voice-dictation hook (useDictation): the error
// messages shown to the user when the mic permission is denied, when the voice
// binary/model isn't ready, or when recording fails to start, plus the generic
// dictation-error fallback rendered in the composer. The Italian column
// reproduces the app's existing copy verbatim. Device/logic strings (status
// enums, console logs, the whisper language code) are NOT translated.
export default {
  it: {
    errorGeneric: 'Errore di dettatura.',
    binaryMissing: 'Binario vocale (whisper-server) non trovato. Vedi le istruzioni di setup.',
    modelNotReady: 'Modello vocale non ancora pronto. Scaricalo prima di dettare.',
    permissionDenied: 'Permesso microfono negato. Abilitalo nelle impostazioni di sistema.',
    startFailed: 'Impossibile avviare la dettatura.',
  },
  en: {
    errorGeneric: 'Dictation error.',
    binaryMissing: 'Voice binary (whisper-server) not found. See the setup instructions.',
    modelNotReady: 'Voice model not ready yet. Download it before dictating.',
    permissionDenied: 'Microphone permission denied. Enable it in your system settings.',
    startFailed: 'Could not start dictation.',
  },
};
