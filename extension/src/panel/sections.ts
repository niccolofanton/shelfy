// The side panel's sections, in display order. Shared registry S7 (P2 lane rule 2): later lanes
// add theirs here — P2-13 "Sync now" and the folder chooser, P2-15 "Sync all" and the schedule,
// P2-16 "Select", P2-17 "Waiting".

import type { PanelSection } from './section';
import { captureSection } from './sections/capture';
import { connectionSection } from './sections/connection';
import { debugSection } from './sections/debug';
import { queueSection } from './sections/queue';
import { plannerSection } from './sections/planner';
import { tasksSection } from './sections/tasks';
import { syncSection } from './sections/sync';
import { selectSection } from './sections/select';

export const SECTIONS: readonly PanelSection[] = [
  connectionSection,
  // P2-13: "Sync now" and the folder chooser.
  syncSection,
  plannerSection,
  selectSection,
  tasksSection,
  captureSection,
  queueSection,
  debugSection,
];
