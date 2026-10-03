// Golden set for the Pinterest hosts the extension is built for
// (extension/src/shared/hosts.ts): its manifest's host permissions and
// content-script matches. The ingest sanitizer allows the same hosts for
// Pinterest URLs, from its own copy (`PINTEREST_HOSTS` in
// crates/core/src/ingest/hosts.rs), and the Rust check compares that copy
// with this file: a host added to one list and not the other fails either
// `run.ts --check` or the Rust golden test.

import { PINTEREST_HOSTS } from '../../extension/src/shared/hosts';
import type { GoldenSet } from './lib';

const hostsSet: GoldenSet = {
  name: 'hosts',
  source: 'extension/src/shared/hosts.ts#PINTEREST_HOSTS',
  generator: 'scripts/golden/hosts.ts',
  build: () => [{ id: 'pinterest-hosts', args: ['pinterest'], output: [...PINTEREST_HOSTS] }],
};

export default hostsSet;
