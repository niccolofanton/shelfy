// Dichiarazioni ambient condivise da tutti i progetti TS di Shelfy.

// Virtual module iniettato da vite.config.ts (buildTimePlugin).
declare module 'virtual:build-time' {
  export const buildTime: number;
}

// Asset importati come URL dal renderer (Vite li risolve a stringa).
declare module '*.woff2' {
  const src: string;
  export default src;
}
declare module '*.woff' {
  const src: string;
  export default src;
}
declare module '*.png' {
  const src: string;
  export default src;
}
declare module '*.svg' {
  const src: string;
  export default src;
}
declare module '*.css';
