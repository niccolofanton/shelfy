// The gradual two-pass scroll of the desktop sync (gradualScroll in src/lib/browserScripts.ts),
// ported to run in the syncing tab's content script (plan §2.16: long work lives in the tab).
// The page's scroll position and DOM are shared with the ISOLATED world, so it scrolls from here;
// the MAIN-world helpers react to the scroll events (X's DOM scan, main/passive.ts).
//
// It steps down ~0.55 viewport at a time and settles, so lazy-loaded tiles load and the page
// fetches its next pages through the hook. When a step cannot advance it pulls the last tile into
// view and waits 500 ms longer. The second pass starts again from the top. A pass ends after 60
// steps without growth (no new item, no progress), or after 3 stalls of 20 s without a capture
// while stuck. The step cap (16,000) and the deadline (30 min) span both passes and the replay.
//
// Growth is counted here from what the bridge relays (distinct items) instead of the hook's
// MAIN-world `__ssCapturedOrder`, which an ISOLATED script cannot read.

export const SCROLL_RULES = {
  passes: 2,
  stallMs: 20_000,
  maxStalls: 3,
  noGrowthLimit: 60,
  stepFraction: 0.55,
  stuckExtraMs: 500,
  passGapMs: 900,
} as const;

/** The last tile per platform: the desktop's SCROLL_SCRIPTS selectors. */
export const LAST_TILE_SELECTOR = {
  instagram: 'a[href^="/p/"]',
  twitter: 'article[data-testid="tweet"]',
  pinterest: 'div[data-test-id="pin"], div[data-grid-item="true"], a[href*="/pin/"]',
} as const;

export interface ScrollEnv {
  scrollY(): number;
  innerHeight(): number;
  scrollBy(dy: number): void;
  scrollTo(y: number): void;
  /** Scrolls the last element matching `selector` into view, if any. */
  revealLast(selector: string): void;
  sleep(ms: number): Promise<void>;
  now(): number;
  /** Distinct items relayed since the sync started. */
  captured(): number;
  /** When the hook last relayed something (a page, or an end-of-feed signal). */
  lastInterceptAt(): number;
  /**
   * Called after every step: true ends the scroll (a stop, a cap, an end-of-feed, the
   * incremental stop). It may wait (a page boundary asks the worker for the known run).
   */
  afterStep(): Promise<boolean>;
}

export interface ScrollOptions {
  selector: string;
  settleMs: number;
  /** Steps left for this run (the cap is shared with the rest of the run). */
  maxSteps: number;
  /** Absolute time the run must end by. */
  deadline: number;
}

export type ScrollOutcome = 'bottom' | 'stopped' | 'step_cap' | 'time_cap';

export interface ScrollResult {
  outcome: ScrollOutcome;
  steps: number;
}

export async function gradualScroll(env: ScrollEnv, options: ScrollOptions): Promise<ScrollResult> {
  const rules = SCROLL_RULES;
  let steps = 0;
  for (let pass = 0; pass < rules.passes; pass++) {
    if (pass > 0) {
      env.scrollTo(0);
      await env.sleep(rules.passGapMs);
    }
    let stalls = 0;
    let noGrowth = 0;
    let lastCount = env.captured();
    for (;;) {
      if (steps >= options.maxSteps) return { outcome: 'step_cap', steps };
      if (env.now() >= options.deadline) return { outcome: 'time_cap', steps };
      steps += 1;
      const beforeY = env.scrollY();
      env.scrollBy(Math.floor(env.innerHeight() * rules.stepFraction));
      let extra = 0;
      if (env.scrollY() <= beforeY + 2) {
        env.revealLast(options.selector);
        extra = rules.stuckExtraMs;
      }
      await env.sleep(options.settleMs + extra);
      if (await env.afterStep()) return { outcome: 'stopped', steps };
      const count = env.captured();
      const advanced = env.scrollY() > beforeY + 2;
      if (count > lastCount || advanced) noGrowth = 0;
      else if (++noGrowth >= rules.noGrowthLimit) break; // the bottom of this pass
      if (count > lastCount) lastCount = count;
      // A stall only while stuck and without fresh captures: never while still advancing.
      if (!advanced && env.now() - env.lastInterceptAt() > rules.stallMs) {
        if (++stalls >= rules.maxStalls) break;
      } else stalls = 0;
    }
  }
  return { outcome: 'bottom', steps };
}
