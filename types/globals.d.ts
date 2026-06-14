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

// eslint-config-prettier ships no type declarations; it is a flat-config object
// consumed by eslint.config.ts.
declare module 'eslint-config-prettier' {
  import type { Linter } from 'eslint';
  const config: Linter.Config;
  export default config;
}
