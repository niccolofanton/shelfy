// The AI tags explorer: reads, health, merge/rename, and the cluster/alias
// review jobs (web port plan §2.9 Tags, App. A's `aitags:*` row). Maps to
// `GET /tags/overview`, `/tags`, `/entities`, `/tags/{tag}/related`,
// `/tags/health`, `/tags/merge-suggestions`, `POST /tags/rename`,
// `/tags/merge`, `/tags/post-keys`, `GET /tag-clusters`, `/tag-aliases` and
// their mutation routes (P3-04, P3-06).
import type {
  AliasStatusResult,
  CancelledResult,
  ClusterStatusResult,
  MergeTagsResult,
  PostSearchResult,
  ProposeAliasesResult,
  RemoveTagResult,
} from '../../../types/electron-api';

// Progress ticks for the long-running cluster/alias LLM jobs (desktop:
// `aitags:clusterProgress` / `aitags:aliasProgress`, opaque run summaries).
export type AiClusterProgress = unknown;
export type AiAliasProgress = unknown;

export interface AiTagsApi {
  getOverview(): Promise<Shelfy.AiOverview>;
  getTagStats(args?: { limit?: number; tier?: Shelfy.TagTier | null }): Promise<Shelfy.Tag[]>;
  getEntityStats(args?: { limit?: number }): Promise<Shelfy.Entity[]>;
  getTagCooccurrence(tag: string, limit?: number): Promise<Shelfy.TagCount[]>;
  getHealth(): Promise<Shelfy.TagHealth>;
  getMergeSuggestions(args?: { limit?: number }): Promise<Shelfy.TagMergeSuggestion[]>;

  renameTag(from: string, to: string): Promise<MergeTagsResult>;
  mergeTags(sources: string[], target: string): Promise<MergeTagsResult>;

  // `GET /tag-clusters`; the review mutations (`PATCH`/`DELETE .../{id}`,
  // `.../{id}/tags/{tag}`).
  getClusters(args?: { maxClusters?: number }): Promise<Shelfy.TagCluster[]>;
  // Runs the clustering job; `onProgress` streams for the call's duration only
  // (the desktop subscribes right before the call and unsubscribes after).
  regenerateClusters(onProgress?: (p: AiClusterProgress) => void): Promise<unknown>;
  cancelClusters(): Promise<CancelledResult>;
  acceptCluster(id: number): Promise<ClusterStatusResult>;
  dismissCluster(id: number): Promise<ClusterStatusResult>;
  renameCluster(id: number, label: string): Promise<ClusterStatusResult>;
  removeTagFromCluster(tag: string, clusterId: number): Promise<RemoveTagResult>;

  // `GET /tag-aliases?status`; `.../accept`, `.../dismiss`, `.../accept-all`.
  getAliases(args?: { status?: Shelfy.AliasStatus | null }): Promise<Shelfy.TagAlias[]>;
  proposeAliases(onProgress?: (p: AiAliasProgress) => void): Promise<ProposeAliasesResult>;
  cancelAliases(): Promise<CancelledResult>;
  acceptAlias(aliasNorm: string): Promise<AliasStatusResult>;
  acceptAllAliases?(): Promise<{ accepted: number; rewritten: number }>;
  dismissAlias(aliasNorm: string): Promise<AliasStatusResult>;

  // `POST /tags/post-keys` (bulk selector by tag match, AND/OR).
  getPostIdsByTags(tags: string[], mode?: 'and' | 'or'): Promise<string[]>;
  // The explorer's raw post lookup (desktop: `db:getPosts`'s ad hoc filter bag,
  // e.g. `{ entity }`). `ShelfyClient.listPosts` covers the general paginated
  // gallery path; this is the tags explorer's own uncapped query.
  getPosts(filters?: unknown): Promise<PostSearchResult>;
}
