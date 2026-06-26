import { useEffect, useRef, useState } from 'react';

export type SearchPhase = 'idle' | 'out' | 'in';

// The exact order a search/filter MUST animate in:
//   1. user searches
//   2. results are fetched in the BACKGROUND while the current results stay up
//   3. the current results all disappear        (phase 'out')
//   4. the search results appear                (phase 'in')
//
// Enforced with a `displayed` buffer kept separate from the live `posts`:
//   • while the query is in flight we HOLD `displayed` on the old results;
//   • once results land we play 'out', THEN swap `displayed` and play 'in'.
// Renderers draw `displayed` (NEVER raw `posts`) and react to `phase`.
//
// A new search DURING an animation does NOT cut it off: it's queued (`awaiting`)
// and only started once the current sequence returns to 'idle', coalesced onto the
// freshest results — so the animation flows from one state to the next instead of
// snapping. Pagination / live patches (no query change) sync straight through.

export interface SearchSequenceOptions {
  outMs?: number; // 'out' duration before the swap — cover the view's exit anim
  settleMs?: number; // hold (still 'out', hidden) AFTER the swap, before revealing
  inMs?: number; // 'in' window — cover the view's entrance anim before idle
}

const DEFAULTS = { outMs: 200, settleMs: 0, inMs: 380 };
const HOLD_CAP_MS = 1500; // failsafe: never hold the old results forever

export interface SearchSequence<T> {
  displayed: T[];
  phase: SearchPhase;
}

export function useSearchSequence<T extends { id: string }>(
  posts: T[],
  loading: boolean,
  queryKey: string,
  options: SearchSequenceOptions = {},
): SearchSequence<T> {
  const [displayed, setDisplayed] = useState<T[]>(posts);
  const [phase, setPhase] = useState<SearchPhase>('idle');

  const prevKey = useRef(queryKey);
  const mounted = useRef(false);
  const awaiting = useRef(false); // a search is queued, waiting to start its sequence
  const phaseRef = useRef<SearchPhase>('idle');
  phaseRef.current = phase;
  const postsRef = useRef(posts);
  postsRef.current = posts;
  const optsRef = useRef(options);
  optsRef.current = options;
  const timers = useRef<number[]>([]);
  const clearTimers = (): void => {
    timers.current.forEach((id) => window.clearTimeout(id));
    timers.current = [];
  };

  // The ordered out → (swap) → [settle] → in → idle run. Reads postsRef at SWAP
  // time (not now): `loading` can flip false a render before usePosts' transitional
  // setPosts commits, so capturing here would swap in the STALE list.
  const runSequence = (): void => {
    const { outMs, settleMs, inMs } = { ...DEFAULTS, ...optsRef.current };
    clearTimers();
    setPhase('out'); // step 3: current results disappear
    timers.current.push(
      window.setTimeout(() => {
        const reveal = (): void => {
          setPhase('in'); // step 4: results appear
          timers.current.push(window.setTimeout(() => setPhase('idle'), inMs));
        };
        if (settleMs > 0) {
          // Swap WHILE hidden, let the new tiles/images paint, THEN reveal.
          setDisplayed(postsRef.current);
          timers.current.push(window.setTimeout(reveal, settleMs));
        } else {
          // Swap + reveal in one commit, so new cards mount straight into 'in'.
          setDisplayed(postsRef.current);
          reveal();
        }
      }, outMs),
    );
  };

  // Query changed → queue a search. Don't start here — `posts`/`loading` aren't
  // ready yet; the effect below starts it. Cap so we never hold forever.
  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      prevKey.current = queryKey;
      return undefined;
    }
    if (prevKey.current === queryKey) return undefined;
    prevKey.current = queryKey;
    awaiting.current = true;
    const cap = window.setTimeout(() => {
      if (!awaiting.current) return;
      awaiting.current = false;
      setDisplayed(postsRef.current);
      setPhase('idle');
    }, HOLD_CAP_MS);
    return () => window.clearTimeout(cap);
  }, [queryKey]);

  // Start (or advance) the queued search when it's allowed to: results in, AND the
  // previous sequence has returned to idle (so a new search flows in after the
  // current one instead of cutting it). Also keeps `displayed` synced when idle and
  // nothing is queued (initial load / paging / live patch).
  useEffect(() => {
    if (!awaiting.current) {
      if (phaseRef.current === 'idle') setDisplayed(posts);
      return undefined;
    }
    if (loading) return undefined; // step 2: still fetching → keep old results up
    if (phaseRef.current !== 'idle') return undefined; // let the current run finish
    awaiting.current = false;
    runSequence();
    return undefined;
  }, [posts, loading, phase]);

  useEffect(() => () => clearTimers(), []);

  return { displayed, phase };
}
