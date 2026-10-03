// MAIN-world entry of debug builds (`build.ts --debug`): the regular hook, then the request
// census wrapped around it (outermost, so it counts every request the hook sees). Release builds
// do not contain the census at all.

import './hook.main';
import { installCensus } from '../main/census';

installCensus(window);
