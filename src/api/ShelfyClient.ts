// The seam between the UI and its backend (web port plan §2.19, D19). Views
// talk to a ShelfyClient instead of `window.electronAPI`, so the same React UI
// runs in the desktop app (electronClient: IPC to the main process) and in the
// browser (web/src/api/httpClient: the HTTP API of shelfy-server).
//
// The interface grows view by view: it holds the operations the migrated views
// need, in transport-neutral terms. Posts keep the desktop shape
// (Shelfy.Post): the web client maps the API's posts onto it, with the post
// `key` as `id` and same-origin `/media` URLs as the local file references.

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
  // AI: the analysis queue, the AI views, search suggestions and the model setup.
  ai: boolean;
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
  concepts?: string[];
  // How `search` and `concepts` combine: 'or' (default) or 'and'.
  conceptMode?: string;
  // 'newest' (default) or 'oldest'. With search text, results rank by relevance.
  sortOrder?: string;
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

// Live changes to the library, pushed by the backend.
export type ShelfyEvent =
  // Posts were added or changed (sync, capture, import…). `count` and
  // `platform` describe a batch of new posts when the backend knows them.
  | { type: 'posts.changed'; count?: number; platform?: string }
  // A post's media finished landing (stored / downloaded).
  | { type: 'post.stored'; postId: string | null }
  // A post's AI analysis finished.
  | { type: 'post.analyzed'; postId: string | null };

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

export interface ShelfyClient {
  readonly capabilities: ShelfyCapabilities;
  readonly media: MediaUrls;

  // One page of the library (or of a folder, a search…).
  listPosts(query: PostQuery, page: PageRequest): Promise<PostPage>;
  // The posts with these ids, in the same order; unknown ids are skipped.
  getPostsByIds(ids: string[]): Promise<Shelfy.Post[]>;
  getStats(): Promise<Shelfy.Stats>;
  // Every folder ("source") with its post count.
  listCollections(): Promise<Shelfy.Collection[]>;

  // Opens an http(s) URL outside the app (a new browser tab on the web).
  openExternal(url: string): void;

  // Subscribes to one kind of live event; returns the unsubscribe function.
  on<T extends ShelfyEventType>(type: T, listener: (event: ShelfyEventOf<T>) => void): () => void;
}
