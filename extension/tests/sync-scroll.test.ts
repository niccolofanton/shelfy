// The sync controller's pure parts (P2-13): the gradual two-pass scroll ported from the desktop
// (content/sync/scroll.ts) on a virtual clock, and the termination rules (termination.ts).

import { describe, expect, it } from 'vitest';
import { SCROLL_SCRIPTS } from '../../src/lib/browserScripts';
import { checkPage, reachedKnownRun } from '../src/content/sync/termination';
import {
  LAST_TILE_SELECTOR,
  SCROLL_RULES,
  gradualScroll,
  type ScrollEnv,
  type ScrollOptions,
} from '../src/content/sync/scroll';

/** A page `height` px tall in a 1000 px viewport, on a virtual clock that sleeps advance. */
function page(height: number, extra: Partial<ScrollEnv> = {}) {
  const state = {
    y: 0,
    now: 0,
    captured: 0,
    lastInterceptAt: 0,
    revealed: 0,
    sleeps: [] as number[],
  };
  const env: ScrollEnv = {
    scrollY: () => state.y,
    innerHeight: () => 1000,
    scrollBy: (dy) => (state.y = Math.min(Math.max(0, height - 1000), state.y + dy)),
    scrollTo: (y) => (state.y = y),
    revealLast: () => void (state.revealed += 1),
    sleep: async (ms) => {
      state.sleeps.push(ms);
      state.now += ms;
    },
    now: () => state.now,
    captured: () => state.captured,
    lastInterceptAt: () => state.lastInterceptAt,
    afterStep: async () => false,
    ...extra,
  };
  return { env, state };
}

const options = (extra: Partial<ScrollOptions> = {}): ScrollOptions => ({
  selector: LAST_TILE_SELECTOR.twitter,
  settleMs: 750,
  maxSteps: 16_000,
  deadline: Number.MAX_SAFE_INTEGER,
  ...extra,
});

describe('gradualScroll', () => {
  it('keeps the desktop rules: selectors, settle times, stall and growth limits', () => {
    for (const platform of ['instagram', 'twitter', 'pinterest'] as const)
      expect(SCROLL_SCRIPTS[platform]).toContain(JSON.stringify(LAST_TILE_SELECTOR[platform]));
    const script = SCROLL_SCRIPTS.instagram;
    expect(script).toContain(`STALL_MS = ${SCROLL_RULES.stallMs}`);
    expect(script).toContain(`MAX_STALLS = ${SCROLL_RULES.maxStalls}`);
    expect(script).toContain(`NO_GROWTH_LIMIT = ${SCROLL_RULES.noGrowthLimit}`);
    expect(script).toContain(`PASSES = ${SCROLL_RULES.passes}`);
    expect(script).toContain(`window.innerHeight * ${SCROLL_RULES.stepFraction}`);
    expect(script).toContain(`extra = ${SCROLL_RULES.stuckExtraMs}`);
    expect(script).toContain(`sleep(${SCROLL_RULES.passGapMs})`);
    expect(SCROLL_SCRIPTS.instagram).toContain('SETTLE_MS = 650');
    expect(SCROLL_SCRIPTS.twitter).toContain('SETTLE_MS = 750');
    expect(SCROLL_SCRIPTS.pinterest).toContain('SETTLE_MS = 650');
  });

  it('walks two passes down a page that stops growing, then reports the bottom', async () => {
    const { env, state } = page(5_500);
    // Captures arrive while the page advances, so no pass ends on a stall.
    const advance = env.scrollBy;
    env.scrollBy = (dy) => {
      advance(dy);
      state.lastInterceptAt = state.now;
    };
    const result = await gradualScroll(env, options());
    // 9 steps reach the bottom (4500 px at 550 px a step), then 60 stuck steps end each pass.
    expect(result).toEqual({ outcome: 'bottom', steps: 2 * (9 + SCROLL_RULES.noGrowthLimit) });
    expect(state.revealed).toBe(2 * SCROLL_RULES.noGrowthLimit);
    expect(state.sleeps).toContain(SCROLL_RULES.passGapMs);
    expect(state.sleeps).toContain(750 + SCROLL_RULES.stuckExtraMs);
  });

  it('ends a pass after three stalls of 20 s without captures while stuck', async () => {
    const { env } = page(1_000); // nothing to scroll: every step is stuck
    const result = await gradualScroll(env, options({ settleMs: 650 }));
    // Each stuck step takes 1150 ms: the first pass stalls at steps 18, 19 and 20 (20 s without
    // a capture); the second starts 20 s late and stalls on its first three steps.
    expect(result).toEqual({ outcome: 'bottom', steps: 20 + 3 });
  });

  it('a new capture resets the growth count', async () => {
    let step = 0;
    const { env, state } = page(1_000, {
      afterStep: async () => {
        step += 1;
        state.lastInterceptAt = state.now;
        if (step % 50 === 0) state.captured += 1; // a capture every 50 stuck steps
        return step >= 500;
      },
    });
    const result = await gradualScroll(env, options());
    expect(result).toEqual({ outcome: 'stopped', steps: 500 });
  });

  it('stops at the step cap and at the deadline, across passes', async () => {
    const capped = page(1_000_000);
    expect(await gradualScroll(capped.env, options({ maxSteps: 25 }))).toEqual({
      outcome: 'step_cap',
      steps: 25,
    });
    const timed = page(1_000_000);
    expect(await gradualScroll(timed.env, options({ deadline: 7_500 }))).toEqual({
      outcome: 'time_cap',
      steps: 10,
    });
  });

  it('stops when afterStep says so', async () => {
    let calls = 0;
    const { env } = page(1_000_000, { afterStep: async () => ++calls === 3 });
    expect(await gradualScroll(env, options())).toEqual({ outcome: 'stopped', steps: 3 });
  });
});

describe('checkPage', () => {
  const folder = 'instagram:ig_collection:17890000000000001';
  const folderUrl = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';

  it('keeps the listing and the post details opened over it', () => {
    expect(checkPage('instagram', folder, folderUrl)).toBe('ok');
    expect(checkPage('instagram', folder, 'https://www.instagram.com/p/ABCdef123/')).toBe('ok');
    expect(checkPage('instagram', folder, 'https://www.instagram.com/reel/ABCdef123/')).toBe('ok');
    expect(checkPage('twitter', 'twitter:x_bookmarks', 'https://x.com/i/bookmarks')).toBe('ok');
    expect(
      checkPage(
        'pinterest',
        'pinterest:pin_board:someone/recipes',
        'https://www.pinterest.com/pin/9001/',
      ),
    ).toBe('ok');
  });

  it('ends on a login wall, per platform', () => {
    expect(checkPage('instagram', folder, 'https://www.instagram.com/accounts/login/?next=x')).toBe(
      'login_required',
    );
    expect(checkPage('twitter', 'twitter:x_bookmarks', 'https://x.com/i/flow/login')).toBe(
      'login_required',
    );
    expect(
      checkPage(
        'pinterest',
        'pinterest:pin_board:someone/recipes',
        'https://www.pinterest.it/login/',
      ),
    ).toBe('login_required');
  });

  it('ends when the tab leaves the listing', () => {
    expect(checkPage('instagram', folder, 'https://www.instagram.com/someone/saved/')).toBe('left');
    expect(
      checkPage(
        'instagram',
        folder,
        'https://www.instagram.com/someone/saved/cakes/17890000000000009/',
      ),
    ).toBe('left');
    expect(checkPage('twitter', 'twitter:x_bookmarks', 'https://x.com/home')).toBe('left');
    expect(checkPage('twitter', 'twitter:x_bookmarks', 'https://x.com/i/history')).toBe('ok');
    expect(
      checkPage(
        'pinterest',
        'pinterest:pin_board:someone/recipes',
        'https://www.pinterest.com/someone/',
      ),
    ).toBe('left');
  });
});

describe('reachedKnownRun', () => {
  it('trusts only a settled count at or above the threshold', () => {
    expect(reachedKnownRun({ streak: 10, settled: true }, 10)).toBe(true);
    expect(reachedKnownRun({ streak: 9, settled: true }, 10)).toBe(false);
    expect(reachedKnownRun({ streak: 50, settled: false }, 10)).toBe(false);
    expect(reachedKnownRun(null, 10)).toBe(false);
    expect(reachedKnownRun({ streak: 1, settled: true }, 0)).toBe(true);
  });
});
