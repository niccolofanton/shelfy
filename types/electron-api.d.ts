// Contratto del bridge preload ⇄ renderer (`window.electronAPI`).
//
// Questa interfaccia è la SINGLE SOURCE OF TRUTH del confine IPC. Viene
// popolata durante l'ondata di conversione di `electron/` leggendo
// `electron/preload.ts` (le firme) e `electron/ipc.ts` (i tipi di ritorno
// reali). `electron/preload.ts` implementa l'oggetto come `ElectronAPI`, così
// implementazione e contratto restano allineati a compile-time.
//
// Finché l'ondata electron non la completa, resta vuota: i file del renderer
// vengono convertiti DOPO electron, quando questa interfaccia è già piena.

export interface ElectronAPI {
  // popolata nell'ondata electron (vedi electron/preload.ts)
}

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}
