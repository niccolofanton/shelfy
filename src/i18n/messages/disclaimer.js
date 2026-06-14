// Strings for the legal disclaimer gate (first-run blocking acknowledgement and
// the read-only review from Settings → Note legali). This is legal text: the
// English column is a faithful, complete translation of the Italian. The full
// bilingual notice lives in DISCLAIMER.md and is shown verbatim via the toggle.
export default {
  it: {
    title: 'Avvertenze legali — leggi prima di usare Shelfy',
    // {strong} markers are rendered as <strong> in the component; keep them.
    bannerPre: 'Shelfy archivia ',
    bannerOnly: 'solo',
    bannerMid: ' i contenuti che ',
    bannerYou: 'tu',
    bannerMid2: ' hai salvato nei ',
    bannerYour: 'tuoi',
    bannerMid3: ' account, per ',
    bannerPersonal: 'uso personale e privato',
    bannerMid4: '. L’utilizzo avviene ',
    bannerRisk: 'a tuo rischio',
    bannerEnd: '.',

    tosTitle: 'Termini di Servizio.',
    tosBody1:
      ' L’accesso automatizzato, l’intercettazione delle API interne, il crawling dei feed e il download dei media ',
    tosViolate: 'possono violare i ToS',
    tosBody2: ' di Instagram, X e Pinterest e portare alla ',
    tosSuspend: 'sospensione del tuo account',
    tosBody3: '. Sei l’unico responsabile del rispetto dei termini di ciascuna piattaforma.',

    copyrightTitle: 'Copyright.',
    copyrightBody:
      ' I contenuti catturati appartengono ai rispettivi titolari, non a te né all’autore. Sei l’unico responsabile della liceità della copia, conservazione e uso di contenuti di terzi.',

    privacyTitle: 'Privacy / GDPR.',
    privacyBody1:
      ' I contenuti possono contenere dati personali di terzi. L’uso personale e locale ricade di norma nell’esenzione domestica; un uso professionale, commerciale o pubblicato ',
    privacyNo: 'no',
    privacyBody2: ', e ti rende titolare del trattamento.',

    warrantyTitle: 'Nessuna garanzia.',
    warrantyBody1:
      ' Il software è fornito «così com’è», senza garanzie. Nei limiti di legge, l’autore non è responsabile di alcun danno, azione sull’account o perdita derivante dall’uso, e tu accetti di ',
    warrantyIndemnify: 'manlevarlo',
    warrantyBody2: '.',

    affiliationTitle: 'Nessuna affiliazione.',
    affiliationBody:
      ' Shelfy è indipendente e non è affiliato, approvato o sponsorizzato da Instagram, Meta, X o Pinterest. I marchi appartengono ai rispettivi titolari.',

    hideFull: 'Nascondi il testo completo',
    showFull: 'Leggi il testo completo (EN / IT)',
    acceptedOn: 'Accettato il {date} · versione {version}',

    checkboxAccept:
      'Ho letto e accetto le avvertenze legali e mi assumo la piena responsabilità del mio utilizzo di Shelfy.',
    dontShowAgain: 'Non mostrare più questo avviso all’avvio',
    acceptAndContinue: 'Accetto e continuo',
  },
  en: {
    title: 'Legal notice — read before using Shelfy',
    bannerPre: 'Shelfy stores ',
    bannerOnly: 'only',
    bannerMid: ' the content that ',
    bannerYou: 'you',
    bannerMid2: ' saved in ',
    bannerYour: 'your',
    bannerMid3: ' accounts, for ',
    bannerPersonal: 'personal and private use',
    bannerMid4: '. You use it ',
    bannerRisk: 'at your own risk',
    bannerEnd: '.',

    tosTitle: 'Terms of Service.',
    tosBody1:
      ' Automated access, interception of internal APIs, crawling feeds and downloading media ',
    tosViolate: 'may violate the ToS',
    tosBody2: ' of Instagram, X and Pinterest and lead to the ',
    tosSuspend: 'suspension of your account',
    tosBody3: '. You are solely responsible for complying with each platform’s terms.',

    copyrightTitle: 'Copyright.',
    copyrightBody:
      ' Captured content belongs to its respective owners, not to you or the author. You are solely responsible for the lawfulness of copying, storing and using third-party content.',

    privacyTitle: 'Privacy / GDPR.',
    privacyBody1:
      ' Content may contain personal data of third parties. Personal, local use generally falls under the household exemption; professional, commercial or published use does ',
    privacyNo: 'not',
    privacyBody2: ', and makes you the data controller.',

    warrantyTitle: 'No warranty.',
    warrantyBody1:
      ' The software is provided «as is», without warranties. To the extent permitted by law, the author is not liable for any damage, account action or loss arising from its use, and you agree to ',
    warrantyIndemnify: 'hold them harmless',
    warrantyBody2: '.',

    affiliationTitle: 'No affiliation.',
    affiliationBody:
      ' Shelfy is independent and is not affiliated with, endorsed or sponsored by Instagram, Meta, X or Pinterest. Trademarks belong to their respective owners.',

    hideFull: 'Hide full text',
    showFull: 'Read the full text (EN / IT)',
    acceptedOn: 'Accepted on {date} · version {version}',

    checkboxAccept:
      'I have read and accept the legal notice and take full responsibility for my use of Shelfy.',
    dontShowAgain: 'Don’t show this notice at startup again',
    acceptAndContinue: 'I accept and continue',
  },
};
