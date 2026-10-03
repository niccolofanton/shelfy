// When a sync must end because of where the tab went (plan §2.16, the desktop's Browser view and
// useSourceSync): a login wall ends it `login_required` (LOGIN_PATTERNS); leaving the listing ends
// it as the user's stop, except for a post detail opened over the grid (isPostDetail), which
// keeps the listing mounted behind a modal. Pure: the controller checks it every step.

import { LOGIN_PATTERNS, isPostDetail } from '../../../../src/lib/browserUrls';
import { syncTarget } from '../../shared/listing';
import type { Platform } from '../../shared/protocol';

export type PageCheck = 'ok' | 'login_required' | 'left';

export function checkPage(platform: Platform, listingKey: string, href: string): PageCheck {
  if (LOGIN_PATTERNS[platform].test(href)) return 'login_required';
  if (isPostDetail(href)) return 'ok';
  return syncTarget(platform, href)?.key === listingKey ? 'ok' : 'left';
}

/**
 * The incremental stop (P2-G1): at a page boundary, stop once the trailing run of known items
 * reaches the threshold. Only a count the worker could settle (every item ingested) is trusted:
 * with the API unreachable the walk goes on, which costs pages but never loses posts.
 */
export function reachedKnownRun(
  answer: { streak: number; settled: boolean } | null,
  stopAfterKnown: number,
): boolean {
  return !!answer && answer.settled && answer.streak >= Math.max(1, stopAfterKnown);
}
