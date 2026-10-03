// useMediaQuery(query) — whether a CSS media query matches, kept current with
// a change listener. False where matchMedia doesn't exist (jsdom, old hosts).
// NARROW_QUERY is Tailwind's `narrow:` screen (tailwind.config.ts).
import { useEffect, useState } from 'react';

export const NARROW_QUERY = '(max-width: 899px)';

function matches(query: string): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(query).matches
    : false;
}

export function useMediaQuery(query: string): boolean {
  const [match, setMatch] = useState(() => matches(query));
  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return undefined;
    const list = window.matchMedia(query);
    const onChange = (): void => setMatch(list.matches);
    onChange();
    list.addEventListener('change', onChange);
    return () => list.removeEventListener('change', onChange);
  }, [query]);
  return match;
}
