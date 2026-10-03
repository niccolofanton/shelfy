/* global document, localStorage, navigator */
// Sets <html lang> before the app's JavaScript runs (UX audit §3.5), with the
// same choice as getInitialLang() in src/i18n/index.tsx: a saved language
// wins, then the browser's (Italian or English), then Italian. React keeps it
// in sync afterwards. A file rather than an inline script: the server's CSP
// allows scripts from 'self' only.
(function () {
  var lang = 'it';
  try {
    var saved = localStorage.getItem('app:language');
    if (saved === 'it' || saved === 'en') {
      lang = saved;
    } else {
      var nav = (navigator.language || '').toLowerCase();
      if (nav.indexOf('en') === 0) lang = 'en';
    }
  } catch {
    /* storage unavailable: keep the default */
  }
  document.documentElement.lang = lang;
})();
