// The desktop ShelfyClient: every operation goes over the preload bridge
// (`window.electronAPI`) exactly as the views called it before the seam, so
// the desktop's behavior does not change.
import type { ElectronAPI } from '../../types/electron-api';
import { assetThumbUrl, assetUrl, isAssetUrl } from '../lib/asset';
import type {
  CollectionDeleteOptions,
  CollectionDeleteResult,
  MediaUrls,
  PageRequest,
  PostEdit,
  PostPage,
  PostQuery,
  ShelfyCapabilities,
  ShelfyClient,
  ShelfyEventOf,
  ShelfyEventType,
} from './ShelfyClient';

// A push payload of the download / analyze channels: only these fields are read.
interface JobProgress {
  status?: string;
  postId?: string | null;
}

const DESKTOP_MEDIA: MediaUrls = {
  file: assetUrl,
  tile: assetThumbUrl,
  isStored: (src) => isAssetUrl(src),
};

// The desktop can do everything but a server account; only the window chrome
// depends on the OS.
export function desktopCapabilities(platform: string | undefined): ShelfyCapabilities {
  return {
    windowControls: platform !== 'darwin',
    trafficLights: platform === 'darwin',
    localFiles: true,
    browser: true,
    webviewFallback: true,
    ai: true,
    websites: true,
    bookmarks: true,
    libraryEdit: true,
    bulkActions: true,
    settings: true,
    activity: true,
    feedback: true,
    account: false,
    updates: true,
    localModels: true,
    links: false,
  };
}

// The desktop pages by offset: the cursor is the offset of the next page.
function offsetOf(cursor: string | null | undefined): number {
  const n = cursor ? Number(cursor) : 0;
  return Number.isInteger(n) && n > 0 ? n : 0;
}

// `bridge` is read on every call, never captured: the preload (or a test)
// may install `window.electronAPI` after this module loads.
export function createElectronClient(
  bridge: () => ElectronAPI = () => window.electronAPI,
): ShelfyClient {
  let capabilities: ShelfyCapabilities | null = null;

  return {
    get capabilities(): ShelfyCapabilities {
      // The platform is fixed for the process lifetime: resolve it once.
      if (!capabilities) capabilities = desktopCapabilities(bridge()?.platform);
      return capabilities;
    },

    media: DESKTOP_MEDIA,

    async listPosts(query: PostQuery, { limit, cursor }: PageRequest): Promise<PostPage> {
      const offset = offsetOf(cursor);
      const result = await bridge().getPosts({ ...query, limit, offset });
      // A malformed IPC result (undefined, or no `posts`) reads as an empty page.
      const posts = Array.isArray(result?.posts) ? result.posts : [];
      const total = result?.total ?? 0;
      const end = offset + posts.length;
      return { posts, total, nextCursor: posts.length > 0 && end < total ? String(end) : null };
    },

    async getPostsByIds(ids: string[]): Promise<Shelfy.Post[]> {
      const rows = await bridge().getPostsByIds(ids);
      return Array.isArray(rows) ? rows : [];
    },

    getStats: () => bridge().getStats(),

    listCollections: () => bridge().getCollections(),

    // The desktop has no single round-trip that both applies a manual edit and
    // returns the post: `updatePostUserContent`/`updatePostAiAnalysis` answer
    // void, so re-read the post after, the same way ActionsMenu's own
    // refreshPost() already does.
    async updatePost(id: string, edit: PostEdit): Promise<Shelfy.Post> {
      const api = bridge();
      if (edit.userNote !== undefined || edit.userTags !== undefined) {
        await api.updatePostUserContent(id, { note: edit.userNote, manualTags: edit.userTags });
      }
      if (
        edit.aiDescription !== undefined ||
        edit.aiTags !== undefined ||
        edit.aiSaveReason !== undefined
      ) {
        await api.updatePostAiAnalysis(id, {
          description: edit.aiDescription,
          tags: edit.aiTags,
          saveReason: edit.aiSaveReason,
        });
      }
      const [fresh] = await api.getPostsByIds([id]);
      if (!fresh) throw new Error(`updatePost: post ${id} is gone`);
      return fresh;
    },

    createCollection: (name, color) => bridge().createCollection(name, color ?? '#3d5afe'),

    updateCollection: (id, fields) => bridge().updateCollection(id, fields),

    async deleteCollection(
      id: number,
      options?: CollectionDeleteOptions,
    ): Promise<CollectionDeleteResult> {
      return bridge().deleteCollection(id, options);
    },

    async addPostsToCollections(postIds: string[], collectionIds: number[]): Promise<void> {
      await bridge().addPostsToCollections(postIds, collectionIds);
    },

    removePostFromCollection: (postId, collectionId) =>
      bridge().removePostFromCollection(postId, collectionId),

    openExternal(url: string): void {
      void bridge().openExternal?.(url);
    },

    on<T extends ShelfyEventType>(type: T, listener: (event: ShelfyEventOf<T>) => void) {
      const api = bridge();
      const emit = listener as (event: ShelfyEventOf<ShelfyEventType>) => void;
      let off: (() => void) | undefined;
      if (type === 'posts.changed') {
        // interceptor:newPosts doubles as a generic "reload the list" signal:
        // only a numeric count and a platform describe a batch of new posts.
        off = api.onNewPosts?.((data: unknown) => {
          const payload = (data ?? {}) as { count?: unknown; platform?: unknown };
          const count = Number(payload.count);
          emit({
            type: 'posts.changed',
            count: Number.isFinite(count) ? count : undefined,
            platform: typeof payload.platform === 'string' ? payload.platform : undefined,
          });
        });
      } else if (type === 'stats.changed') {
        // No IPC channel carries the counters: like the app before the seam,
        // every interceptor:newPosts may have moved them (plan App. A:
        // interceptor:newPosts → posts.changed + stats.changed).
        off = api.onNewPosts?.(() => emit({ type: 'stats.changed' }));
      } else if (type === 'post.stored' || type === 'post.analyzed') {
        // Only a finished job changes what a post shows; progress ticks don't.
        const subscribe = type === 'post.stored' ? api.onDownloadProgress : api.onAnalyzeProgress;
        off = subscribe?.((data: unknown) => {
          const job = data as JobProgress | null;
          if (job?.status !== 'done') return;
          emit({ type, postId: job.postId ?? null } as ShelfyEventOf<ShelfyEventType>);
        });
      }
      return () => {
        if (typeof off === 'function') off();
      };
    },

    // The desktop has no crash endpoint: the report goes to the console.
    reportError({ view, error, componentStack }): void {
      console.error(`[ErrorBoundary] ${view}:`, error, componentStack ?? '');
    },
  };
}

let shared: ShelfyClient | null = null;

// The desktop app's client, shared by the app root and by components rendered
// without a ShelfyProvider (the desktop component tests).
export function getElectronClient(): ShelfyClient {
  if (!shared) shared = createElectronClient();
  return shared;
}
