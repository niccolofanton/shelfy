// useReducedMotion() — true when the user asks the system for less motion
// (UX audit §3.2). CSS animations already stop under the blanket rule in
// src/index.css; this is for what CSS can't reach: JavaScript animation,
// smooth scrolling (`behavior: reduced ? 'auto' : 'smooth'`) and video
// autoplay. It follows the setting live.
import { useMediaQuery } from '../components/ui/useMediaQuery';

export const REDUCED_MOTION_QUERY = '(prefers-reduced-motion: reduce)';

export function useReducedMotion(): boolean {
  return useMediaQuery(REDUCED_MOTION_QUERY);
}
