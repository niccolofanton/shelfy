// Shared UI→API filter mapping for the post surfaces. usePosts uses it to build
// the fetch query; Gallery uses it to resolve the full matching id set behind
// "Seleziona tutti" (whose result feeds the bulk actions, including delete).
// A single source of truth guarantees the bulk set is exactly the displayed set:
// 'all' sentinels become undefined and the UI-only 'linkonly' status maps to the
// backend's 'missing'.

// UI-side filter state consumed by both the Gallery view and usePosts. Every
// field is optional here so either caller's (structurally similar) filter object
// can be passed without coercion.
export interface UiFilters {
  platform?: string;
  source?: string;
  mediaType?: string;
  downloadStatus?: string;
  search?: string;
  collectionId?: number | null;
  category?: string;
  contentType?: string;
  tag?: string;
  aiTagged?: string;
  concepts?: string[];
  conceptMode?: string;
  sortOrder?: string;
}

// The normalized query the db:getPosts / db:getPostIds handlers consume: 'all'
// sentinels stripped to undefined, 'linkonly' remapped to the backend's 'missing'.
export interface ApiFilters {
  platform: string | undefined;
  source: string | undefined;
  mediaType: string | undefined;
  downloadStatus: string | undefined;
  search: string | undefined;
  collectionId: number | null | undefined;
  category: string | undefined;
  contentType: string | undefined;
  tag: string | undefined;
  aiTagged: string | undefined;
  concepts: string[] | undefined;
  conceptMode: string | undefined;
  sortOrder: string | undefined;
}

export function toApiFilters(filters: UiFilters = {}): ApiFilters {
  return {
    platform: filters.platform !== 'all' ? filters.platform : undefined,
    source: filters.source && filters.source !== 'all' ? filters.source : undefined,
    mediaType: filters.mediaType !== 'all' ? filters.mediaType : undefined,
    downloadStatus:
      filters.downloadStatus && filters.downloadStatus !== 'all'
        ? filters.downloadStatus === 'linkonly'
          ? 'missing'
          : 'downloaded'
        : undefined,
    search: filters.search || undefined,
    collectionId: filters.collectionId || undefined,
    category: filters.category || undefined,
    contentType: filters.contentType || undefined,
    tag: filters.tag || undefined,
    aiTagged: filters.aiTagged && filters.aiTagged !== 'all' ? filters.aiTagged : undefined,
    concepts: filters.concepts && filters.concepts.length ? filters.concepts : undefined,
    conceptMode: filters.conceptMode || undefined,
    sortOrder: filters.sortOrder || undefined,
  };
}
