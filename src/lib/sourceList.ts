// The library's sources — "All posts", the four platform folders and any
// custom folder — in one place, shared by the Sidebar and the gallery's
// filter drawer (plan §1.2 #13, P1.md P1-06/P1-14) so both list the exact
// same sources, in the same order, instead of each keeping its own copy.
import { Globe, Instagram, type LucideIcon } from 'lucide-react';
import PinterestIcon from '../components/PinterestIcon';
import { XIcon } from '../components/SourceIcon';

// Lucide-compatible icon component: the subset of props a source row passes
// (size + className only). Covers both lucide-react icons and PinterestIcon.
export type SourceIcon = (props: { size?: number; className?: string }) => React.ReactNode;

export interface PlatformSource {
  id: string;
  // A verbatim brand name, or an i18n key resolved against the `sidebar`
  // namespace at render (only the non-brand 'web' row is translated).
  label?: string;
  key?: string;
  Icon: SourceIcon | LucideIcon;
}

// Fixed order: the three synced platforms, then websites. Manual bookmarks
// have no folder of their own — the full media-type facets (#13) cover them.
export const PLATFORM_SOURCES: PlatformSource[] = [
  { id: 'instagram', label: 'Instagram', Icon: Instagram },
  // The X logo, not lucide's Twitter bird (UX audit SH-9). The label stays
  // "X / Twitter" while the transition lasts (audit §3.7).
  { id: 'twitter', label: 'X / Twitter', Icon: XIcon },
  { id: 'pinterest', label: 'Pinterest', Icon: PinterestIcon },
  { id: 'web', key: 'web', Icon: Globe },
];

// Folders nested under one platform row (e.g. an Instagram saved folder).
export function collectionsForPlatform(
  collections: Shelfy.Collection[],
  platformId: string,
): Shelfy.Collection[] {
  return collections.filter((c) => c.platform === platformId);
}

// Folders that belong to no known platform: plain custom folders, plus any
// orphan whose `platform` is outside PLATFORM_SOURCES (legacy/unknown ids) —
// so a collection is never invisible.
export function customCollections(collections: Shelfy.Collection[]): Shelfy.Collection[] {
  return collections.filter((c) => !PLATFORM_SOURCES.some((p) => p.id === c.platform));
}
