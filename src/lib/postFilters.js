// Shared UI→API filter mapping for the post surfaces. usePosts uses it to build
// the fetch query; Gallery uses it to resolve the full matching id set behind
// "Seleziona tutti" (whose result feeds the bulk actions, including delete).
// A single source of truth guarantees the bulk set is exactly the displayed set:
// 'all' sentinels become undefined and the UI-only 'linkonly' status maps to the
// backend's 'missing'.
export function toApiFilters(filters = {}) {
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
