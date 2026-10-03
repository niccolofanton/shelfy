// What build.ts bakes into the bundles (esbuild `define`): the Shelfy origin the build talks to
// (`--origin`), whether it is a debug build (`--debug`), and the id of the bundled capture hook,
// sent as `client.parser` with every ingest batch (C5). Unbuilt code (vitest) gets the defaults.

import { DEFAULT_SHELFY_ORIGIN } from './hosts';

declare const __SHELFY_ORIGIN__: string | undefined;
declare const __SHELFY_DEBUG__: boolean | undefined;
declare const __SHELFY_PARSER__: string | undefined;

export interface BuildInfo {
  /** The Shelfy origin, without a trailing slash. */
  origin: string;
  debug: boolean;
  /** `<first 12 hex of the SHA-256 of electron/webview-injected.ts>`, or `dev` when unbuilt. */
  parser: string;
}

export const BUILD: BuildInfo = {
  origin: typeof __SHELFY_ORIGIN__ === 'string' ? __SHELFY_ORIGIN__ : DEFAULT_SHELFY_ORIGIN,
  debug: typeof __SHELFY_DEBUG__ === 'boolean' ? __SHELFY_DEBUG__ : false,
  parser: typeof __SHELFY_PARSER__ === 'string' ? __SHELFY_PARSER__ : 'dev',
};
