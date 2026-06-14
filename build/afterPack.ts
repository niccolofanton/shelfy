'use strict';

// afterPack hook — disattiva le Electron "fuses" pericolose nell'app pacchettizzata.
//
// Senza questo passo l'eseguibile Electron resta utilizzabile come runtime Node
// generico (ELECTRON_RUN_AS_NODE), accetta NODE_OPTIONS e gli argomenti di debug
// `--inspect`: tutto ciò permette di aggirare il codice firmato dell'app ed
// eseguire script arbitrari con l'identità dell'app. Qui spegniamo quelle fuse.
//
// Gira PRIMA di afterSign: flippare le fuse invalida la firma, quindi è corretto
// rifirmare dopo (lo fa build/afterSign.cjs su macOS). Su macOS chiediamo a
// flipFuses di riapplicare la firma ad-hoc così il binario resta caricabile tra i
// due hook; afterSign la sovrascrive comunque con l'identifier corretto del bundle.

import path from 'path';
import fs from 'fs';
import { flipFuses, FuseVersion, FuseV1Options } from '@electron/fuses';
import type { AfterPackContext } from 'electron-builder';

export default async function afterPack(context: AfterPackContext): Promise<void> {
  const { electronPlatformName, appOutDir, packager } = context;
  const productFilename = packager.appInfo.productFilename;

  let electronBinaryPath: string;
  if (electronPlatformName === 'darwin') {
    electronBinaryPath = path.join(appOutDir, `${productFilename}.app`);
  } else if (electronPlatformName === 'win32') {
    electronBinaryPath = path.join(appOutDir, `${productFilename}.exe`);
  } else {
    // Linux: electron-builder nomina l'eseguibile con `executableName` (default: il
    // `name` minuscolo del package, es. "shelfy"), NON il productFilename ("SHELFY").
    // Proviamo i nomi noti e scegliamo quello realmente presente, così la fuse non
    // viene applicata a un path inesistente né saltata in silenzio (sarebbe un buco
    // di sicurezza). Se nessuno esiste, lasciamo che flipFuses fallisca forte.
    const executableName = (packager as { executableName?: string }).executableName;
    const candidates = [
      executableName,
      packager.appInfo.name,
      productFilename,
      productFilename.toLowerCase(),
    ].filter((n): n is string => Boolean(n));
    electronBinaryPath =
      candidates.map((n) => path.join(appOutDir, n)).find((p) => fs.existsSync(p)) ||
      path.join(appOutDir, productFilename);
  }

  console.log(`[afterPack] disattivo le fuse pericolose: ${electronBinaryPath}`);
  await flipFuses(electronBinaryPath, {
    version: FuseVersion.V1,
    [FuseV1Options.RunAsNode]: false,
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable]: false,
    [FuseV1Options.EnableNodeCliInspectArguments]: false,
    [FuseV1Options.OnlyLoadAppFromAsar]: true,
    resetAdHocDarwinSignature: electronPlatformName === 'darwin',
  });
  console.log('[afterPack] fuse applicate.');
}
