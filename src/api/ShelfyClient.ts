// The seam between the UI and its backend (web port plan §2.19, D19). Views
// talk to a ShelfyClient instead of `window.electronAPI`, so the same React UI
// runs in the desktop app (electronClient: IPC to the main process) and in the
// browser (web/src/api/httpClient: the HTTP API of shelfy-server).
//
// The interface grows view by view: it holds the operations the migrated views
// need, in transport-neutral terms. Posts keep the desktop shape
// (Shelfy.Post): the web client maps the API's posts onto it, with the post
// `key` as `id` and same-origin `/media` URLs as the local file references.
import type { ActivityApi } from './activity';
import type { AccountApi } from './account';
import type { AiApi } from './ai';
import type { JobsApi } from './jobs';
import type { LinksApi } from './links';

// What a client can do. The UI hides what its client cannot do instead of
// checking for `window.electronAPI` (plan §2.19 Capabilities). The desktop has
// everything; the web client turns features on as the server gains them.
export interface ShelfyCapabilities {
  // Custom minimize / maximize / close buttons (frameless window on Windows and Linux).
  windowControls: boolean;
  // Native macOS traffic lights over the sidebar, which keeps a drag strip for them.
  trafficLights: boolean;
  // Media on this machine: the download queue, local copies (open, reveal,
  // delete) and the preview cache.
  localFiles: boolean;
  // The in-app platform browsers: Connections, source sync and hand-picked saves.
  browser: boolean;
  // The original page, live, inside the post modal when no media can be shown.
  webviewFallback: boolean;
  // AI: true when any of the five flags below is (plan §2.19; P3-08
  // Assumption G3-21 — §2.19 names one `ai.tasks` capability for several
  // views that land at different times, so each area gets its own flag and
  // `ai` reads as "any of them"). Consumers that only care whether SOME AI
  // feature is on (e.g. AiPanel.tsx) read this one; a specific view or hook
  // reads its own flag.
  ai: boolean;
  // The analyze queue (src/hooks/useAnalysis.tsx, src/views/AiTagsQueue.tsx):
  // `useShelfy().ai.queue`.
  aiQueue: boolean;
  // The AI Tags explorer (src/views/AiTags.tsx, src/hooks/useAiTags.ts):
  // `useShelfy().ai.tags`.
  aiTags: boolean;
  // AI chat search (src/views/AiSearch.tsx, src/hooks/useAiSearch.ts):
  // `useShelfy().ai.search`.
  aiChat: boolean;
  // Gallery search-suggestion chips (AI-41, P3-21): `useShelfy().ai.suggest`.
  aiSuggest: boolean;
  // Voice dictation for the AI-search composer (src/hooks/useDictation.ts):
  // `useShelfy().ai.dictation`.
  dictation: boolean;
  // Website capture and the Websites view.
  websites: boolean;
  // Manual bookmarks made from local files.
  bookmarks: boolean;
  // Edits: notes, manual tags, manual AI edits, folders and folder membership.
  libraryEdit: boolean;
  // Selection, bulk actions and post deletion.
  bulkActions: boolean;
  // The Settings view.
  settings: boolean;
  // The Activity center of background work.
  activity: boolean;
  // The feedback form.
  feedback: boolean;
  // A signed-in account on a server (ShelfyClient.account): passkeys,
  // sessions, tokens, storage, and settings and consent kept on the server.
  account: boolean;
  // App updates: the update channel and the installer (the desktop's updater).
  updates: boolean;
  // Local AI models on this machine: the model pickers, their runtime
  // binaries and the performance tuning.
  localModels: boolean;
  // Turning a URL into a post from outside the app (ShelfyClient.links):
  // `/share` (Android's share target, the bookmarklet), the iOS Shortcut.
  // Web only; the desktop has no `/share` page.
  links: boolean;
  // The Jobs view (ShelfyClient.jobs): the web's background-job manager,
  // which replaces the desktop's Downloads there (PG18). Off on the desktop,
  // which keeps its own download queue and local files instead.
  jobs: boolean;
}

// The gallery query, as toApiFilters (src/lib/postFilters.ts) normalizes it:
// 'all' sentinels are already gone. Pagination travels separately (PageRequest).
export interface PostQuery {
  platform?: string;
  // 'web' (websites) or 'social' (everything else).
  source?: string;
  mediaType?: string;
  // 'downloaded' (a local / stored copy exists) or 'missing'.
  downloadStatus?: string;
  search?: string;
  collectionId?: number | null;
  category?: string;
  contentType?: string;
  tag?: string;
  // 'tagged' or 'untagged' (AI tags only).
  aiTagged?: string;
  // The post's exact AI status ('pending' | 'analyzing' | 'done' | 'error').
  aiStatus?: string;
  concepts?: string[];
  // How `search` and `concepts` combine: 'or' (default) or 'and'.
  conceptMode?: string;
  // 'newest' (default) or 'oldest'. With search text, results rank by relevance.
  sortOrder?: string;
  // The trash instead of the library (P1-11/P1-14). Desktop queries never set
  // this: it has no trash.
  trash?: boolean;
}

export interface PageRequest {
  // Posts wanted in this page.
  limit: number;
  // `nextCursor` of the previous page; absent or null for the first page.
  cursor?: string | null;
  // Aborts the request when a newer query replaces it.
  signal?: AbortSignal;
}

export interface PostPage {
  posts: Shelfy.Post[];
  // Posts matching the query over all pages. A first page always carries it; a
  // later page may not (keep the first page's value then).
  total?: number;
  // Cursor of the next page; null on the last page.
  nextCursor: string | null;
}

// What changed posts, when the backend says (the web API's reasons).
export type PostsChangeReason =
  | 'ingest'
  | 'archive'
  | 'ai'
  | 'edit'
  | 'delete'
  | 'capture'
  | 'import';

// Live changes to the library, pushed by the backend.
export type ShelfyEvent =
  // Posts were added, changed or removed (sync, capture, import, edits…).
  // `count` and `platform` describe a batch of new posts when the backend
  // knows them (the desktop); `keys` names the changed posts when they are
  // few (the web: at most 200, null for more) and `reason` says what changed.
  | {
      type: 'posts.changed';
      count?: number;
      platform?: string;
      keys?: string[] | null;
      reason?: PostsChangeReason;
    }
  // The library's counters changed: reload the stats.
  | { type: 'stats.changed' }
  // Live changes were lost (the web stream fell behind, or the server
  // restarted): reload everything on screen. The desktop's IPC loses nothing
  // and never sends it.
  | { type: 'resync' }
  // A post's media finished landing (stored / downloaded).
  | { type: 'post.stored'; postId: string | null }
  // A post's AI analysis finished.
  | { type: 'post.analyzed'; postId: string | null }
  // A background job moved on (P1-14: progress for a bulk/trash/purge action
  // over 500 posts). Minimal on purpose — P4-09 owns the full jobs seam and
  // may extend or replace this for the Jobs view. The desktop has no job
  // system for these actions and never sends it.
  | {
      type: 'job.updated';
      id: number;
      kind: string;
      state: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
      progress: number | null;
      errorCode: string | null;
    };

// The AI events: `post.analyzed` above is the one cross-cutting AI signal
// (any queue item finishing) — consumers that just need to know "something
// finished, reload" (src/hooks/useAiTags.ts's live refresh) subscribe to it
// here. Every other AI progress stream (queue items, cluster/alias runs, chat
// tokens, model downloads) is per-area and finer-grained than this coarse,
// replayed union, so it lives on its own `AiApi` area instead
// (`ai.queue.onProgress`, `ai.search.onToken`, …) — see src/api/ai/*.ts.

export type ShelfyEventType = ShelfyEvent['type'];
export type ShelfyEventOf<T extends ShelfyEventType> = Extract<ShelfyEvent, { type: T }>;

// Turns a post's local file references (thumbnailPath, imagePath, videoPath,
// media[].localPath) into URLs the UI can load. On the desktop they are paths
// served by the asset:// protocol; on the web, same-origin /media URLs.
export interface MediaUrls {
  // The stored file, full size.
  file(ref: string | null | undefined): string | null;
  // A rendition for grid tiles, about `width` px wide, else the file itself.
  tile(ref: string | null | undefined, width?: number): string | null;
  // Whether `src` points at a stored copy rather than a remote URL.
  isStored(src: string | null | undefined): boolean;
}

// An error that a view's error boundary caught (src/components/ErrorBoundary.tsx).
export interface ViewErrorReport {
  // The view whose boundary caught it: 'gallery', 'postModal', 'settings'…
  view: string;
  // What was thrown.
  error: unknown;
  // React's component stack, when known.
  componentStack?: string | null;
}

// A manual edit saved in one call (plan P1-03: `PATCH /posts/{key}` covers all
// of it): the user-authored layer (note, manual tags) and/or a hand-edited AI
// field (the desktop's `updateAiAnalysis`; the server records its model as
// `manual`, where the desktop wrote `manuale`). Absent fields are left alone;
// `null` clears one. Every field maps 1:1 onto the matching `Shelfy.Post` one.
export interface PostEdit {
  userNote?: string | null;
  userTags?: string[] | null;
  aiDescription?: string | null;
  aiTags?: string[] | null;
  aiSaveReason?: string | null;
}

// Options for deleting a folder (§1.2 #12). `deletePosts` also moves every
// post currently in it to the trash (server-side since P1-11); the UI offers
// the choice only once its own trash/bulk surface exists (`bulkActions`, P1-14).
export interface CollectionDeleteOptions {
  deletePosts?: boolean;
}

// A background job a bulk/trash/purge action queued instead of running inline
// (over 500 posts, P1-11); its progress arrives as the `job.updated` event.
export interface BulkJob {
  id: number;
  kind: string;
}

// What deleting a folder changes, in the desktop IPC's terms (kept so the
// existing CollectionModal reads either client's answer the same way).
export interface CollectionDeleteResult {
  ok: boolean;
  deletedPosts: number;
  errors: string[];
  // Present with `deletePosts`: the undo handle for `restoreFromTrash`
  // (`{deletedAt}`), or null when nothing moved. Optional: the desktop's
  // delete is immediate and permanent, so it never has one (F11: a null stamp
  // means nothing to undo).
  deletedAt?: number | null;
  // Present when a large folder's posts are queued as a job instead of
  // trashed inline (F11). Optional for the same reason as `deletedAt`.
  job?: BulkJob | null;
}

// What a bulk action does to the selected posts (plan §2.9). `analyze`,
// `fetchMedia` and `removeStoredMedia` are not part of this seam yet: the
// server answers them 422 `not_available` until P2-P4, and the UI hides the
// actions that would call them behind their own capability (`ai`, `localFiles`).
export type BulkActionKind =
  | 'delete'
  | 'restore'
  | 'addToCollections'
  | 'removeFromCollection'
  | 'clearAiDescription'
  | 'clearAiTags';

// The arguments a bulk action takes, for the ones that take some.
export interface BulkActionParams {
  // `removeFromCollection`: the collection, by id.
  collectionId?: number;
  // `addToCollections`: the collections, by id.
  collectionIds?: number[];
}

// Which posts a bulk/trash action applies to: explicit keys (the UI chunks a
// selection over 200 into several calls, so one request never carries more),
// or every post `filter` matches minus `exceptKeys` ("select all matching":
// P1-03/P1-11's `{filter, exceptKeys}`). `filter.trash` targets the trash.
export interface BulkSelector {
  keys?: string[];
  filter?: PostQuery;
  exceptKeys?: string[];
}

// The outcome of a bulk/restore action: what it changed (the request ran it),
// or the job that is running it (a selection over 500 posts).
export interface BulkOutcome {
  // Posts the action actually changed; null while a job is still running it.
  changed: number | null;
  // Posts the selector matched when the action started.
  selected: number;
  // `delete` (and a folder's `mode=withPosts`): the undo handle for
  // `restoreFromTrash`. Null when nothing was moved — offer no "Undo" then
  // (F11: a null stamp means nothing to undo).
  deletedAt: number | null;
  // Non-null when the server queued a job instead of running inline.
  job: BulkJob | null;
}

// One page of the trash (newest delete first).
export interface TrashPage {
  posts: Shelfy.Post[];
  total: number;
  // Days a post stays in the trash before the nightly purge deletes it.
  retentionDays: number;
  nextCursor: string | null;
}

// Bringing posts back from the trash: by key, by a filter over the trash
// (minus some), or by one delete's undo handle.
export type RestoreSelector = BulkSelector | { deletedAt: number };

export interface ShelfyClient {
  readonly capabilities: ShelfyCapabilities;
  readonly media: MediaUrls;
  // The signed-in account (src/api/account.ts), when the backend has one: the
  // web client's. The desktop has none.
  readonly account?: AccountApi;
  // Turning a URL into a post (src/api/links.ts), when the backend has one:
  // the web client's, once signed in. The desktop has none.
  readonly links?: LinksApi;
  // The AI seam (src/api/ai/), present when any `ai*`/`dictation` capability
  // is on: the desktop always has it; the web gains it area by area (P3-11,
  // P3-17, P3-18, P3-20, P3-22) as each capability flag turns on.
  readonly ai?: AiApi;
  // The account's background jobs (src/api/jobs.ts), when the backend has
  // one: the web client's. The desktop keeps its own Downloads queue instead
  // (ShelfyCapabilities.jobs) and leaves this undefined.
  readonly jobs?: JobsApi;
  readonly activity?: ActivityApi;

  // One page of the library (or of a folder, a search…).
  listPosts(query: PostQuery, page: PageRequest): Promise<PostPage>;
  // The posts with these ids, in the same order; unknown ids are skipped.
  getPostsByIds(ids: string[]): Promise<Shelfy.Post[]>;
  getStats(): Promise<Shelfy.Stats>;
  // Every folder ("source") with its post count.
  listCollections(): Promise<Shelfy.Collection[]>;

  // How many posts `query` matches, over every page: the gallery's count
  // pill, and the authoritative size of a "select all matching" (P1-03/P1-05:
  // cheap — it shares the server's cache with `listPosts`'s `includeTotal`).
  countPosts(query: PostQuery): Promise<number>;
  // Resolves every id `query` matches, when the client can do that cheaply
  // (the desktop's local SQLite); null tells the caller to select by filter
  // instead (`BulkSelector`), because materializing the ids would not scale
  // (the web, P1-11/P1-14).
  resolveAllIds(query: PostQuery): Promise<string[] | null>;
  // Runs one action on the posts `selector` names (plan §2.9, P1-11). Up to
  // 500 posts run inline; a larger selection becomes a job (`outcome.job`),
  // reported by the `job.updated` event.
  bulkAction(
    selector: BulkSelector,
    action: BulkActionKind,
    params?: BulkActionParams,
  ): Promise<BulkOutcome>;
  // The trash, newest delete first.
  listTrash(page: PageRequest): Promise<TrashPage>;
  restoreFromTrash(selector: RestoreSelector): Promise<BulkOutcome>;
  // Starts the purge that empties the trash (a job: its progress arrives as
  // `job.updated`).
  emptyTrash(): Promise<{ selected: number; job: BulkJob | null }>;

  // Saves a manual edit and returns the post after the change (plan P1-03),
  // which an optimistic update reconciles onto, or rolls back from on failure.
  updatePost(id: string, edit: PostEdit): Promise<Shelfy.Post>;

  // Folders. `createCollection`/`updateCollection` return what the API does;
  // the desktop client's own write already is (or cheaply becomes) that shape.
  createCollection(name: string, color?: string): Promise<Shelfy.Collection>;
  updateCollection(id: number, fields: { name?: string; color?: string }): Promise<void>;
  deleteCollection(id: number, options?: CollectionDeleteOptions): Promise<CollectionDeleteResult>;
  // Adds every post to every collection given (a cross product, as the
  // desktop's `addPostsToCollections` already behaves); callers in this
  // codebase only ever pass one collection id at a time.
  addPostsToCollections(postIds: string[], collectionIds: number[]): Promise<void>;
  // Takes one post out of one folder (§1.2 #12); not in it is not an error.
  removePostFromCollection(postId: string, collectionId: number): Promise<void>;

  // Opens an http(s) URL outside the app (a new browser tab on the web).
  openExternal(url: string): void;

  // Subscribes to one kind of live event; returns the unsubscribe function.
  on<T extends ShelfyEventType>(type: T, listener: (event: ShelfyEventOf<T>) => void): () => void;

  // Reports an error an error boundary caught. The web client sends it to the
  // server (throttled, technical fields only); the desktop logs it. Never
  // throws.
  reportError(report: ViewErrorReport): void;
}
