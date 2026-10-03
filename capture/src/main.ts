// Bundle entry for the capture service: configure the env and start the server.
// build.ts bundles this into capture/dist/server.cjs (the container CMD).
import { main } from './server';

main();
