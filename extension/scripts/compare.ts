// SPIKE-3 parity check: extension "Export JSON" vs the desktop's capture of the same listings.
//
//   pnpm exec tsx extension/scripts/compare.ts --extension <export.json> \
//     (--desktop-db <shelfy.sqlite> | --desktop-export <saved-posts.json>) [options]
//
// Which desktop data is read, the listing mapping and the matching rules are documented at the
// top of compare-lib.ts and in docs/web-port/spikes/03-extension-capture.md. --help lists options.

import { writeFileSync } from 'node:fs';
import { runCompareCli } from './compare-lib';

process.exitCode = runCompareCli(process.argv.slice(2), {
  stdout: (text) => process.stdout.write(text),
  stderr: (text) => process.stderr.write(text),
  writeFile: (path, content) => writeFileSync(path, content),
});
