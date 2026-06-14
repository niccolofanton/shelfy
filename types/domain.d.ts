// Shared Shelfy domain model — mirrors the SQLite schema in electron/db.js.
// Global (no import needed) so main process and renderer agree on shapes.
//
// SOURCE OF TRUTH: electron/db.js. The interfaces below mirror the camelCase
// shapes the exported query functions actually RETURN (rowToPost, getStats,
// getCollections, getPostsByIds, the tag/cluster/alias/snapshot queries, …),
// NOT the raw snake_case table rows. SQLite has no boolean/array types, so:
//   - INTEGER epoch / count columns surface as `number`;
//   - INTEGER 0/1 flags that the code coerces (`!!…`) surface as `boolean`;
//   - JSON TEXT columns (ai_tags, web_palette_json, …) are parsed back to
//     `string[]` / objects / `| null` by the query layer and typed as such here;
//   - nullable columns are `… | null`; fields only attached by some queries are
//     marked optional `?`.

declare global {
  namespace Shelfy {
    // ── enum-like unions (real values found in electron/db.js) ──────────────

    // posts.platform. 'web' = a saved website (media_type 'website');
    // 'manual' = a user bookmark built from local files; the rest are scrapers.
    export type Platform = 'instagram' | 'twitter' | 'pinterest' | 'web' | 'manual';

    // posts.media_type. 'website' for web refs, 'file' for manual non-AV
    // bookmarks, 'text' for caption-only posts. 'carousel'/'images' appear in
    // importer/source data; the canonical persisted slide types are narrower
    // (see PostMedia.type).
    export type MediaType = 'image' | 'video' | 'text' | 'website' | 'file' | 'carousel' | 'images';

    // posts.ai_status — the local VLM analysis lifecycle. NULL = never analyzed.
    export type AiStatus = 'pending' | 'analyzing' | 'done' | 'error';

    // Per-slide media type persisted in post_media.media_type. The writer
    // (deriveMedia/replacePostMedia) coerces social media to 'image'|'video';
    // manual bookmarks may persist 'file'.
    export type PostMediaType = 'image' | 'video' | 'file';

    // Review lifecycle shared by tag_cluster.status and (a subset of) the
    // cluster mutation API. Dismissed rows are deleted, never stored.
    export type ClusterStatus = 'proposed' | 'accepted' | 'dismissed';

    // tag_alias.status — only 'accepted' aliases canonicalize tags.
    export type AliasStatus = 'proposed' | 'accepted';

    // post_tags.tier — AI tags split into general/specific; manual user tags are
    // tier 'manual'; legacy/untiered rows are NULL.
    export type TagTier = 'general' | 'specific' | 'manual';

    // Background queue kinds persisted in the jobs table.
    export type JobKind = 'download' | 'analyze' | 'web';

    // ── posts ───────────────────────────────────────────────────────────────

    // A `posts` row as returned by rowToPost (camelCase), with `media` and
    // `collectionIds` attached by getPosts/getPostsByIds/getPost. Web-reference
    // and AI fields are NULL/[] for plain social posts.
    export interface Post {
      id: string;
      platform: Platform;
      shortcode: string | null;
      postUrl: string | null;
      profileUrl: string | null;
      authorUsername: string | null;
      authorName: string | null;
      text: string | null;
      thumbnailUrl: string | null;
      mediaType: MediaType | null;
      timestamp: string | null; // ISO 8601 string (lexically sortable)
      thumbnailPath: string | null; // local file path once downloaded
      imagePath: string | null;
      videoPath: string | null;
      thumbBlur: string | null; // blur-up data URI ('' sentinel mapped to null)
      mediaCount: number; // INTEGER, defaults to 1
      importedAt: number; // unix epoch seconds (INTEGER)

      // AI analysis layer (ai_* columns). Arrays are JSON-parsed (never null).
      aiDescription: string | null;
      aiTags: string[];
      aiStatus: AiStatus | null;
      aiModel: string | null;
      aiAnalyzedAt: number | null; // unix epoch seconds
      aiCategory: string | null; // raw enum slug (e.g. industry)
      aiContentType: string | null; // raw enum slug (e.g. purpose)
      aiEntities: string[];
      aiKeywords: string[];
      aiLanguage: string | null;
      aiSaveReason: string | null;

      // User-authored layer (independent of AI; survives regeneration).
      userNote: string | null;
      userTags: string[];

      // Web-reference layer (platform='web'); null/[]/false for social posts.
      webUrl: string | null;
      webDomain: string | null;
      webFinalUrl: string | null;
      webPalette: string[]; // HEX colors
      webFonts: WebFont[];
      webTech: string[];
      webAwards: WebAward[];
      webPages: WebPage[];
      webMeta: WebMeta | null;
      webSinglePage: boolean; // coerced via !! from web_meta_json.singlePage
      webCapturedAt: number | null; // unix epoch seconds

      // Attached by the list/by-id queries (not part of rowToPost itself).
      media?: PostMedia[];
      collectionIds?: number[];

      // Attached only by the JSON export path (attachTagTiers/attachCollectionKeys).
      aiGeneralTags?: string[];
      aiSpecificTags?: string[];
      collections?: string[]; // stable collection keys, export only
    }

    // Lightweight projection returned by getPostsForAnalysis (the analyzer only
    // needs these columns). `media` is attached; no collections, no AI fields.
    export interface AnalysisPost {
      id: string;
      shortcode: string | null;
      mediaType: MediaType | null;
      videoPath: string | null;
      imagePath: string | null;
      thumbnailPath: string | null;
      thumbnailUrl: string | null;
      authorUsername: string | null;
      text: string | null;
      media?: PostMedia[];
    }

    // ── post_media ────────────────────────────────────────────────────────────

    // One slide of a post, as attached by attachMedia (a `post_media` row in
    // camelCase). `position` is the 0-based order; `localPath` is filled by the
    // downloader (NULL until then). source_url surfaces as `url`.
    export interface PostMedia {
      position: number;
      type: PostMediaType;
      url: string | null; // source_url
      localPath: string | null; // local_path
    }

    // The shape deriveMedia / replacePostMedia CONSUME when writing media (input
    // side): no position (assigned by index), localPath optional.
    export interface PostMediaInput {
      type: PostMediaType;
      url: string;
      localPath?: string;
    }

    // ── collections ───────────────────────────────────────────────────────────

    // A `collections` row as returned by getCollections / createCollection
    // (camelCase), with the live post `count`. platform='instagram' marks a tag
    // auto-derived from an IG saved folder; externalId links it back (rename-safe).
    export interface Collection {
      id: number;
      name: string;
      color: string; // HEX, defaults to '#3d5afe'
      platform: Platform | null;
      externalId: string | null;
      igName: string | null;
      count: number;
      createdAt?: number; // unix epoch seconds (present in getCollections)
    }

    // Portable collection definition for JSON export (getCollectionsForExport):
    // no volatile id/count, keyed by externalId (IG folders) or name (manual).
    export interface CollectionExport {
      name: string;
      color: string;
      platform: Platform | null;
      externalId: string | null;
      igName: string | null;
    }

    // ── downloads ─────────────────────────────────────────────────────────────

    // A `downloads` row: one asset-download record per post asset. Mirrors the
    // table columns (snake→camel). status/progress track the in-flight transfer.
    export interface DownloadJob {
      id: number;
      postId: string | null;
      assetType: string; // 'thumbnail' | 'image' | 'video' (free-form in schema)
      status: string; // 'pending' | 'downloading' | 'done' | 'error' | …
      progress: number; // REAL 0..1
      error: string | null;
      startedAt: number | null; // unix epoch seconds
      completedAt: number | null;
    }

    // ── jobs (persisted background queue mirror) ───────────────────────────────

    // A `jobs` row (jobsByKind returns raw snake_case rows). Generic CRUD mirror
    // of the in-memory download/analyze/web queues, keyed by (kind, key).
    export interface Job {
      kind: JobKind;
      key: string; // the manager's jobKey
      post_id: string | null;
      payload: string | null; // JSON: compact job record for resume
      status: string;
      progress: number; // REAL, defaults to 0
      error: string | null;
      attempts: number;
      created_at: number; // unix epoch seconds
      updated_at: number;
    }

    // ── post_tags / post_entities (derived indexes) ────────────────────────────

    // A `post_tags` row: derived index over posts.ai_tags / user_tags. tag_norm
    // is the lowercased canonical key; tag_form keeps a display casing. tier
    // distinguishes AI (general/specific) from manual tags (NULL = legacy).
    export interface PostTag {
      post_id: string;
      tag_norm: string;
      tag_form: string;
      tier: TagTier | null;
    }

    // A `post_entities` row: derived index over posts.ai_entities.
    export interface PostEntity {
      post_id: string;
      ent_norm: string;
      ent_form: string;
    }

    // Aggregated per-tag stats from getTagStats: display form + post count +
    // last-used timestamp + per-category distribution.
    export interface Tag {
      tag: string; // display form (most frequent casing)
      count: number;
      lastUsed: string | null; // ISO timestamp of most recent post
      categories: TagCategoryCount[];
    }

    export interface TagCategoryCount {
      category: string;
      count: number;
    }

    // Aggregated per-entity stats from getEntityStats.
    export interface Entity {
      entity: string; // display form
      count: number;
    }

    // A { tag, count } pair (getTagCooccurrence, searchTagsByText,
    // getTopTagsForTextQuery, getFrequentTags returns the bare strings).
    export interface TagCount {
      tag: string;
      count: number;
    }

    // Distinctiveness-ranked tag from getTagDistinctivenessForTextQuery: how
    // characteristic the tag is of the matched post set vs. the whole archive.
    export interface TagDistinctiveness {
      tag: string;
      inSet: number; // matched posts carrying it
      count: number; // global post count (the lift denominator)
      lift: number; // inSet / count, 0..1
      score: number;
    }

    // ── tag_alias (synonym canonicalization) ───────────────────────────────────

    // A `tag_alias` row as returned by getTagAliases: maps a synonym form
    // (aliasNorm) onto a chosen canonical (canonicalNorm). `count` = posts that
    // would be canonicalized by accepting it (0 once already applied).
    export interface TagAlias {
      aliasNorm: string;
      aliasForm: string; // best display form of the alias in post_tags
      canonicalNorm: string;
      canonicalForm: string;
      status: AliasStatus;
      count: number;
    }

    // Result of resolveAlias: a tag_norm resolved to its canonical { norm, form }.
    export interface ResolvedAlias {
      norm: string;
      form: string;
    }

    // A vocabulary tag from getUnaliasedTags / getCanonicalVocab.
    export interface VocabTag {
      norm: string;
      form: string;
      count: number;
    }

    // Near-duplicate merge suggestion from getTagMergeSuggestions.
    export interface TagMergeSuggestion {
      canonical: string; // most-frequent variant
      variants: string[];
      totalCount: number;
    }

    // ── tag_cluster / tag_cluster_membership ───────────────────────────────────

    // A semantic tag cluster as returned by getTagClusters: the persisted
    // tag_cluster row (id/label/status) resolved to display-form members +
    // post count. `topTag` mirrors the model-given label.
    export interface TagCluster {
      id: number;
      label: string;
      status: 'proposed' | 'accepted';
      topTag: string;
      tags: string[]; // member tags, display form, freq-sorted
      postCount: number;
    }

    // A `tag_cluster_membership` row (one tag → at most one cluster).
    export interface TagClusterMembership {
      tag_norm: string;
      cluster_id: number;
    }

    // Raw co-occurrence candidate group fed to the LLM by getTagCandidateGroups:
    // member tag norms + each member's top co-occurring neighbors.
    export interface TagCandidateGroup {
      tags: string[]; // normalized tag keys
      neighbors: Record<string, string[]>; // norm → top co-occurring norms
    }

    // ── tag graph (getTagGraph) ────────────────────────────────────────────────

    export interface TagGraph {
      nodes: TagGraphNode[];
      edges: TagGraphEdge[];
    }

    export interface TagGraphNode {
      id: string; // tag_norm
      label: string;
      weight: number; // post count
      clusterId: number | null; // accepted-cluster membership, if any
    }

    export interface TagGraphEdge {
      source: string; // tag_norm
      target: string; // tag_norm
      weight: number; // co-occurrence count
    }

    // ── web references ─────────────────────────────────────────────────────────
    //
    // Web-specific column shapes (parsed from the web_*_json TEXT columns).
    // Deliberately loose where the underlying capture data is open-ended.

    export interface WebFont {
      family: string;
      usage?: string;
    }

    export interface WebAward {
      platform: string;
      level?: string;
      date?: string;
      profileUrl?: string;
    }

    // Per-page metadata captured for a site (one entry per crawled page).
    export interface WebPage {
      url: string;
      pageType?: string;
      title?: string;
      meta?: WebPageMeta;
      jsonld?: unknown;
      contentText?: string;
      screenshotPath?: string; // local file, when captured
      chunks?: WebPageChunk[]; // vertical screenshot bands of a tall page
    }

    export interface WebPageChunk {
      screenshotPath?: string;
    }

    export interface WebPageMeta {
      description?: string;
      ogImage?: string;
      lang?: string;
      [key: string]: unknown;
    }

    // Hero/site-level meta object (web_meta_json).
    export interface WebMeta {
      description?: string;
      ogImage?: string;
      lang?: string;
      singlePage?: boolean;
      [key: string]: unknown;
    }

    // An archived version of a site, as returned by getWebSnapshots (a
    // `web_snapshots` row parsed back to camelCase). The posts row holds the
    // CURRENT capture; these are the older ones, newest first.
    export interface WebSnapshot {
      id: number;
      postId: string;
      capturedAt: number; // unix epoch seconds
      title: string | null;
      webPages: WebPage[];
      webPalette: string[];
      webFonts: WebFont[];
      webTech: string[];
      webAwards: WebAward[];
      webMeta: WebMeta | null;
      aiDescription: string | null;
      aiTags: string[];
      aiModel: string | null;
      aiStatus: AiStatus | null;
      aiCategory: string | null;
      aiContentType: string | null;
      aiEntities: string[];
      aiKeywords: string[];
      aiLanguage: string | null;
      aiSaveReason: string | null;
    }

    // ── stats / overview (getStats, getAiOverview, getTagHealth) ───────────────

    // getStats: archive-wide counters. byPlatform always carries the four known
    // platform keys; byMediaType is sparse (only media types present).
    export interface Stats {
      total: number;
      byPlatform: Record<'instagram' | 'twitter' | 'pinterest' | 'web', number>;
      byMediaType: Partial<Record<string, number>>;
      downloaded: number;
      downloadedByType: {
        thumbnails: number;
        images: number;
        videos: number;
      };
    }

    // getAiOverview: header counters for the AI Tags dashboard.
    export interface AiOverview {
      total: number;
      analyzed: number;
      unanalyzed: number;
      byCategory: NamedCount<'category'>[];
      byContentType: NamedCount<'contentType'>[];
      languages: NamedCount<'language'>[];
      uniqueTags: number;
      taggedPosts: number;
    }

    // A { <key>, count } aggregation row (ai_category/ai_content_type/ai_language
    // GROUP BYs in getAiOverview).
    export type NamedCount<K extends string> = { [P in K]: string } & { count: number };

    // getTagHealth: tag-hygiene snapshot.
    export interface TagHealth {
      orphanTags: TagCount[]; // tags used in exactly one post (count always 1)
      rareTags: number; // tags used in <= 2 posts
      unanalyzedPosts: number;
      untaggedPosts: number; // analyzed but produced no tags
    }

    // ── operation results (write-path return shapes) ───────────────────────────

    export interface UpsertResult {
      inserted: number;
      skipped: number;
      aiUpdated: number;
    }

    export interface ImportResult {
      imported: number;
      updated: number;
      collections: number;
      links: number;
    }
  }
}

export {};
