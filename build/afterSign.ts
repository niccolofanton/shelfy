// afterSign hook — firma ad-hoc REALE del .app su macOS.
//
// electron-builder (24.x) con `mac.identity: "-"` NON applica una firma ad-hoc:
// interpreta "-" come nome di un'identità da cercare in keychain, non la trova e
// SALTA la firma, lasciando la vecchia firma linker di Electron (Identifier=Electron)
// che, dopo il repackaging, non valida più → su Apple Silicon l'app risulta
// "danneggiata". Qui ri-firmiamo l'app già pacchettizzata con `codesign --sign -`
// (ad-hoc) e l'identifier corretto del bundle, PRIMA che venga creato il .dmg/.zip.
// La firma ad-hoc è sufficiente ad avviare l'app (l'utente sblocca comunque la
// quarantena Gatekeeper al primo avvio); non sostituisce la notarizzazione.

import path from 'path';
import { execFileSync } from 'child_process';
import type { AfterPackContext } from 'electron-builder';

export default async function afterSign(context: AfterPackContext) {
  if (context.electronPlatformName !== 'darwin') return;
  const { appOutDir, packager } = context;
  const appPath = path.join(appOutDir, `${packager.appInfo.productFilename}.app`);
  const id = packager.appInfo.id || 'com.shelfy.app';
  console.log(`[afterSign] firma ad-hoc: ${appPath} (identifier=${id})`);
  execFileSync('codesign', ['--force', '--deep', '--sign', '-', '--identifier', id, appPath], {
    stdio: 'inherit',
  });
  // Verifica che la firma sia coerente col bundle (fallisce il build se non lo è).
  execFileSync('codesign', ['--verify', '--deep', '--strict', appPath], { stdio: 'inherit' });
  console.log('[afterSign] firma ad-hoc applicata e verificata.');
}
