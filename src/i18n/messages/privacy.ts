// The privacy notice of the web app, version 1 (PRIVACY_VERSION in
// src/disclaimer.ts), drawn from plan §7.2 for the owner-only phase (E4). Users
// accept it with the disclaimer (POST /me/consent); Settings → Legal shows it
// again. This is legal text: the two columns say the same thing. A change of
// substance is a new version, which asks everyone to accept it again; v2
// arrives before invitations (P5).
//
// src/components/PrivacyNotice.tsx renders it: `<name>Title` heads a section,
// `<name>Body` is a paragraph and `<name>1…n` are list items.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Informativa sulla privacy',
    version: 'Versione {version}',
    intro:
      'Questa informativa spiega quali dati conserva questo server Shelfy, dove, per quanto tempo e chi li tratta.',

    whoTitle: 'Chi tratta i tuoi dati',
    whoBody:
      'Questo server Shelfy è gestito privatamente da chi lo amministra, che decide come usarlo e custodisce i tuoi dati: è il titolare del trattamento. Shelfy è software open source: i suoi autori non ricevono né vedono i tuoi dati. Per qualsiasi richiesta scrivi a chi amministra il server.',

    dataTitle: 'Cosa conserva',
    data1:
      'Il tuo account: l’indirizzo email, il ruolo, le impostazioni (lingua, cosa archiviare) e quando hai accettato queste note.',
    data2:
      'Come accedi: le chiavi pubbliche delle tue passkey (impronta, volto o PIN non lasciano mai il tuo dispositivo), le sessioni (inizio, ultimo uso e lo User-Agent del browser) e l’impronta crittografica dei link di accesso e dei token API, mai il loro valore.',
    data3:
      'La tua libreria: i post che salvi, con testi e media di altre persone, le tue note e i tuoi tag e, in futuro, le analisi AI che richiedi.',
    data4:
      'Sicurezza e funzionamento: un registro degli eventi di sicurezza (accessi, modifiche a passkey e token) con l’indirizzo di rete pseudonimizzato, i log tecnici del server e i rapporti di errore dell’app, che contengono solo dettagli tecnici e mai i tuoi post.',
    data5: 'Le copie di backup di tutto questo, cifrate prima di lasciare il server.',

    whereTitle: 'Dove si trovano',
    whereBody:
      'Su un server noleggiato da Hetzner. I backup sono cifrati sul server e poi conservati su Cloudflare R2, che non può leggerli.',

    processorsTitle: 'Chi altro li tratta',
    processors1: 'Hetzner, per l’hosting del server.',
    processors2:
      'Cloudflare, per il DNS, la cifratura in transito e il tunnel che collega il server: il tuo traffico passa dalla rete di Cloudflare. Finché il server è privato, Cloudflare Access verifica chi può raggiungerlo. Cloudflare conserva anche i backup cifrati.',
    processors3: 'Resend, che consegna le email di accesso, solo se l’accesso via email è attivo.',
    processors4:
      'Fornitori AI: nessuno per ora. Quando arriveranno le funzioni AI, i contenuti andranno solo ai fornitori che configuri tu, solo per le azioni che avvii e dopo una schermata di consenso.',

    trackingTitle: 'Niente tracciamento',
    trackingBody:
      'Niente statistiche, pubblicità o telemetria. L’app web non carica nulla da altri siti e usa un solo cookie, per mantenere l’accesso.',

    retentionTitle: 'Per quanto tempo',
    retention1:
      'La libreria e le impostazioni: finché non le elimini, o finché non elimini l’account.',
    retention2: 'Il cestino: 30 giorni.',
    retention3:
      'Le sessioni: finiscono 30 giorni dopo l’ultimo uso, al massimo dopo 90. I link di accesso: 15 minuti, una volta sola.',
    retention4: 'Le attività in background concluse: 14 giorni.',
    retention5: 'Il registro di sicurezza: 1 anno. I log tecnici: pochi giorni.',
    retention6: 'I backup: fino a 6 mesi.',

    rightsTitle: 'I tuoi diritti',
    rightsBody:
      'Puoi vedere e correggere i tuoi dati nell’app. Puoi chiedere a chi amministra il server una copia dei tuoi dati o la cancellazione dell’account: la cancellazione toglie subito la libreria dal server, e dai backup entro 6 mesi, man mano che scadono. Secondo il GDPR puoi anche opporti a un trattamento, chiederne la limitazione e presentare reclamo all’autorità per la protezione dei dati.',

    othersTitle: 'I contenuti di altre persone',
    othersBody:
      'I post che salvi contengono contenuti e dati personali di altre persone. Li conservi per uso personale e privato: le tue responsabilità sono nelle avvertenze legali.',

    changesTitle: 'Modifiche',
    changesBody:
      'Se questa informativa cambia, l’app ti mostra la nuova versione e ti chiede di accettarla di nuovo.',
  },
  en: {
    title: 'Privacy notice',
    version: 'Version {version}',
    intro:
      'This notice explains what this Shelfy server keeps, where, for how long, and who processes it.',

    whoTitle: 'Who handles your data',
    whoBody:
      'This Shelfy server is run privately by its operator, who decides how it is used and looks after your data: the operator is the data controller. Shelfy is open-source software: its authors neither receive nor see your data. For any request, write to the operator of this server.',

    dataTitle: 'What it keeps',
    data1:
      'Your account: your email address, your role, your settings (language, what to archive) and when you accepted these notices.',
    data2:
      'How you sign in: the public keys of your passkeys (your fingerprint, face or PIN never leave your device), your sessions (when they started and were last used, and the browser’s User-Agent), and a cryptographic fingerprint of sign-in links and API tokens, never their value.',
    data3:
      'Your library: the posts you save, with other people’s text and media, your notes and tags and, later, the AI analyses you ask for.',
    data4:
      'Security and operations: a log of security events (sign-ins, passkey and token changes) with a pseudonymized network address, the server’s technical logs, and the app’s crash reports, which carry technical details only and never your posts.',
    data5: 'Backups of all of this, encrypted before they leave the server.',

    whereTitle: 'Where it is',
    whereBody:
      'On a server rented from Hetzner. Backups are encrypted on the server, then stored on Cloudflare R2, which cannot read them.',

    processorsTitle: 'Who else processes it',
    processors1: 'Hetzner, which hosts the server.',
    processors2:
      'Cloudflare, for DNS, encryption in transit and the tunnel that connects the server: your traffic passes through Cloudflare’s network. While the server is private, Cloudflare Access checks who may reach it. Cloudflare also stores the encrypted backups.',
    processors3: 'Resend, which delivers sign-in emails, only if email sign-in is on.',
    processors4:
      'AI providers: none for now. When AI features arrive, content will go only to the providers you set up, only for actions you start, and after a consent screen.',

    trackingTitle: 'No tracking',
    trackingBody:
      'No analytics, advertising or telemetry. The web app loads nothing from other sites and uses a single cookie, to keep you signed in.',

    retentionTitle: 'How long',
    retention1: 'Your library and settings: until you delete them, or delete your account.',
    retention2: 'The trash: 30 days.',
    retention3:
      'Sessions: they end 30 days after their last use, after 90 days at most. Sign-in links: 15 minutes, once.',
    retention4: 'Finished background jobs: 14 days.',
    retention5: 'The security log: 1 year. Technical logs: a few days.',
    retention6: 'Backups: up to 6 months.',

    rightsTitle: 'Your rights',
    rightsBody:
      'You can see and correct your data in the app. You can ask the operator for a copy of your data or to delete your account: deletion removes your library from the server at once, and from the backups within 6 months, as they expire. Under the GDPR you can also object to processing, ask for it to be restricted, and complain to your data protection authority.',

    othersTitle: 'Other people’s content',
    othersBody:
      'The posts you save contain other people’s content and personal data. You keep them for your personal and private use: your responsibilities are in the legal notice.',

    changesTitle: 'Changes',
    changesBody:
      'If this notice changes, the app shows you the new version and asks you to accept it again.',
  },
} satisfies LangMessages;
