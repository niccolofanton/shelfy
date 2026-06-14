const { app, ipcMain, dialog, shell } = require('electron');
const db = require('./db');
const downloader = require('./downloader');
const analyzer = require('./analyzer');
const weborchestrator = require('./weborchestrator');
const stt = require('./stt');
const embeddings = require('./embeddings');
const updater = require('./updater');
const binaries = require('./binaries');
const feedback = require('./feedback');
const bookmarks = require('./bookmarks');
const netSafety = require('./net-safety');
const path = require('path');
const fs = require('fs');

let _window = null;
let _registered = false;
let _chatAbort = null; // AbortController for the in-flight AI-search chat (one at a time)
// Monotonic id stamped on every streamed chat token. Lets the renderer drop
// tokens an aborted previous run flushes after a new run has started (the
// abort takes a moment to propagate to llama-server). See useAiSearch.js.
let _chatRunSeq = 0;
let _clusterAbort = null; // AbortController for the in-flight cluster regeneration
let _aliasAbort = null; // AbortController for the in-flight tag-alias build (P3)
// The single most-recent path the user picked through the app's own open dialog.
// db:importJSON only accepts this path (and clears it after consuming) so the
// renderer can't import arbitrary files and the allow-list can't grow unbounded
// or keep stale paths importable for the whole session.
let _lastPickedImportPath = null;
// Upper bound for renderer-supplied arrays on bulk write/enqueue IPC channels.
// These accept untrusted, webview-originated payloads (the scraper injects
// MAIN-world scripts), so we cap length at the boundary the way stt:transcribe
// caps its buffer size — guarding against a buggy/compromised renderer.
const MAX_BULK_ITEMS = 100000;
// Byte budget for a single bookmark:add invoke: per-file cap (original +
// preview of one entry) and aggregate cap across all files. Mirrors the
// 200MB/file + 500MB total the renderer enforces (src/lib/bookmarkFiles.js)
// and the preload pre-flight — this is the defensive main-side check of that
// contract for a renderer that bypasses both.
const MAX_BOOKMARK_FILE_BYTES = 200 * 1024 * 1024;
const MAX_BOOKMARK_BYTES = 500 * 1024 * 1024;
// Chunk size for the bulk hydrate/enqueue loops (download:all, analyze:*, …):
// each iteration materializes at most this many posts from the DB, then yields
// to the event loop so the main process stays responsive on large libraries.
const BATCH_SIZE = 200;

// Send an event to the current window, guarding against a destroyed window
// (the window can be recreated, e.g. on macOS re-activate).
function sendToWindow(channel, payload) {
  if (_window && !_window.isDestroyed()) {
    _window.webContents.send(channel, payload);
  }
}

// Lazily read & cache the MAIN-world injected capture script (webview-injected.js).
// Only a successful, non-empty read is cached: on failure the cache stays null
// so a later renderer retry re-reads the file instead of getting '' forever.
let _injectedScript = null;
function getWebviewInjectedScript() {
  if (_injectedScript != null) return _injectedScript;
  try {
    const src = fs.readFileSync(path.join(__dirname, 'webview-injected.js'), 'utf8');
    if (src) _injectedScript = src;
    return src;
  } catch (e) {
    console.error('[ipc] Failed to read webview-injected.js:', e.message);
    return '';
  }
}

// Lazily read & cache the MAIN-world selection-overlay script (webview-select.js):
// draws a checkbox + "in libreria" chip over each saved post in the scraper.
// Same retry-friendly caching as getWebviewInjectedScript above.
let _selectScript = null;
function getWebviewSelectScript() {
  if (_selectScript != null) return _selectScript;
  try {
    const src = fs.readFileSync(path.join(__dirname, 'webview-select.js'), 'utf8');
    if (src) _selectScript = src;
    return src;
  } catch (e) {
    console.error('[ipc] Failed to read webview-select.js:', e.message);
    return '';
  }
}

// Resolve a path's real (symlink-free) location. Falls back to the input when
// the path doesn't exist (so it can't be a symlink to an escape target).
function realpathSafe(p) {
  try {
    return fs.realpathSync(p);
  } catch {
    return p;
  }
}

// Confine `filePath` to the app's userData tree, resolving symlinks first so a
// symlink inside userData can't escape the root. Returns the resolved path if
// allowed, or null if outside the tree.
function confineToUserData(filePath) {
  const resolved = realpathSafe(path.resolve(filePath || ''));
  const root = realpathSafe(path.resolve(app.getPath('userData')));
  if (resolved !== root && !resolved.startsWith(root + path.sep)) return null;
  return resolved;
}

function registerIpcHandlers(mainWindow) {
  // Always update the window ref so events go to the current window
  _window = mainWindow;

  // Wire the progress emitter to always use the current window
  downloader.setProgressEmitter((job) => {
    sendToWindow('download:progress', job);
  });
  analyzer.setProgressEmitter((job) => {
    sendToWindow('analyze:progress', job);
  });
  weborchestrator.setProgressEmitter((job) => {
    sendToWindow('web:progress', job);
  });
  // Reuse the existing list-refresh channel so the gallery re-fetches when a web
  // placeholder appears or a reference is promoted (zero new wiring in usePosts).
  weborchestrator.setListRefreshEmitter((payload) => {
    sendToWindow('interceptor:newPosts', payload);
  });

  // Handlers can only be registered once per app lifetime
  if (_registered) return;
  _registered = true;

  // ── DB ──────────────────────────────────────────────────────────────────────

  ipcMain.handle('db:getPosts', (_, filters) => db.getPosts(filters));
  ipcMain.handle('db:getPostIds', (_, filters) => db.getPostIds(filters));
  ipcMain.handle('db:getPostsByIds', (_, { ids }) => db.getPostsByIds(ids));
  ipcMain.handle('db:existingIds', (_, { ids } = {}) => db.existingIds(ids || []));
  ipcMain.handle('db:savedByKeys', (_, { keys } = {}) => db.savedByKeys(keys || []));
  ipcMain.handle('db:getStats', () => db.getStats());
  ipcMain.handle('db:importJSON', async (_, { filePath }) => {
    // Only import the file the user just selected via the app's open dialog. This
    // blocks a compromised/buggy renderer from importing arbitrary filesystem
    // paths. The pick is consumed (cleared) here so it can't be re-imported later
    // and the allow-list can't accumulate stale paths over a long session.
    if (!filePath || filePath !== _lastPickedImportPath) {
      throw new Error('Import non consentito: selezionare un file tramite la finestra di dialogo.');
    }
    // Consume the pick only AFTER a successful import: clearing it before would
    // break the modal's "Riprova" flow (it keeps the same filePath), turning a
    // transient failure (e.g. malformed JSON) into a permanent "not allowed".
    const result = await db.importFromJSON(filePath);
    _lastPickedImportPath = null;
    return result;
  });
  ipcMain.handle('db:exportJSON', async (_, { platforms } = {}) => {
    const result = await dialog.showSaveDialog(
      _window && !_window.isDestroyed() ? _window : undefined,
      {
        defaultPath: 'saved-posts.json',
        filters: [{ name: 'JSON', extensions: ['json'] }],
      },
    );
    if (result.canceled || !result.filePath) return { canceled: true };
    const posts = db.exportAllPosts(platforms);
    const collections = db.getCollectionsForExport();
    // Serialize + write in chunks, yielding between them, instead of one
    // monolithic JSON.stringify of the whole library (which doubles peak memory
    // and blocks the main process for the entire serialization on large DBs).
    const CHUNK = 500;
    let fh = null;
    try {
      fh = await fs.promises.open(result.filePath, 'w');
      await fh.write('{\n"posts": [\n');
      for (let i = 0; i < posts.length; i += CHUNK) {
        const part = posts
          .slice(i, i + CHUNK)
          .map((p) => JSON.stringify(p))
          .join(',\n');
        await fh.write((i > 0 ? ',\n' : '') + part);
        await new Promise((resolve) => setImmediate(resolve));
      }
      await fh.write('\n],\n"collections": ' + JSON.stringify(collections, null, 2) + '\n}\n');
    } catch (e) {
      return { canceled: false, error: e.message };
    } finally {
      if (fh) await fh.close().catch(() => {});
    }
    return { canceled: false, count: posts.length, filePath: result.filePath };
  });
  ipcMain.handle('db:clearAll', async () => {
    // Wiping the library must also stop every background queue and drop their
    // persisted job rows — otherwise in-flight downloads/analysis/web captures
    // keep churning against postIds that no longer exist, the Activity strip
    // keeps spinning, and recover() would resurrect the orphaned jobs on the
    // next boot.
    try {
      await downloader.cancelAll();
    } catch {}
    try {
      analyzer.cancelAll();
      analyzer.clearAll();
    } catch {}
    try {
      weborchestrator.cancelAll();
    } catch {}
    // Also abort the long-running local-LLM jobs this module owns. They read the
    // tags up front, then spend a long time in the model and only persist
    // tag_cluster/tag_alias rows at the very end — without this they'd finish
    // after the wipe and re-populate the just-cleared tables with stale rows.
    // Their .finally() handlers null the refs.
    try {
      if (_chatAbort) _chatAbort.abort();
    } catch {}
    try {
      if (_clusterAbort) _clusterAbort.abort();
    } catch {}
    try {
      if (_aliasAbort) _aliasAbort.abort();
    } catch {}
    db.clearAllData();
    // Truncate any persisted job rows that survived the in-memory cancel so the
    // boot-time recover() can't bring them back.
    try {
      db.jobDeleteAll('download');
      db.jobDeleteAll('analyze');
      db.jobDeleteAll('web');
    } catch {}
    return { ok: true };
  });
  ipcMain.handle('db:clearAiAnalysis', () => {
    // Same race as db:clearAll: abort the in-flight cluster/alias (and chat) jobs
    // this module owns before wiping the derived AI data, otherwise a running job
    // re-writes stale tag_cluster/tag_alias rows after clearAllAiAnalysis() has
    // emptied them. Their .finally() handlers null the refs.
    try {
      if (_chatAbort) _chatAbort.abort();
    } catch {}
    try {
      if (_clusterAbort) _clusterAbort.abort();
    } catch {}
    try {
      if (_aliasAbort) _aliasAbort.abort();
    } catch {}
    return db.clearAllAiAnalysis();
  });
  ipcMain.handle('db:clearAssets', async () => {
    await downloader.clearAllAssets();
    db.clearAllAssetPaths();
    return { ok: true };
  });

  ipcMain.handle('db:deleteLocalFiles', async (_, { postId }) => {
    // Only the CURRENT capture's files: this action frees disk while KEEPING the
    // post record, so it must not touch archived snapshot screenshots (which
    // getLocalFilePaths now includes) — deleting those would orphan their
    // web_snapshots rows. removePostsAndFiles below still uses getLocalFilePaths
    // because it deletes the whole post.
    const paths = db.getCurrentCaptureFilePaths(postId);
    const errors = [];
    for (const p of paths) {
      try {
        fs.unlinkSync(p);
      } catch (e) {
        if (e.code !== 'ENOENT') errors.push(e.message);
      }
    }
    db.clearPostLocalFiles(postId);
    return { ok: true, deleted: paths.length, errors };
  });

  // Permanently delete posts: remove their on-disk files (best-effort), then the
  // DB rows (cascades to media/downloads/collections/tags/entities). Shared by the
  // explicit "delete posts" action and the "delete a collection AND its posts" path.
  const removePostsAndFiles = async (ids = []) => {
    const list = Array.isArray(ids) ? ids.filter(Boolean) : [];
    const errors = [];
    const BATCH = 200;
    for (let i = 0; i < list.length; i += BATCH) {
      for (const id of list.slice(i, i + BATCH)) {
        for (const p of db.getLocalFilePaths(id)) {
          try {
            await fs.promises.unlink(p);
          } catch (e) {
            if (e.code !== 'ENOENT') errors.push(e.message);
          }
        }
      }
      await new Promise((resolve) => setImmediate(resolve));
    }
    const { deleted } = db.deletePosts(list);
    return { deleted, errors };
  };

  ipcMain.handle('db:deletePosts', async (_, { ids }) => {
    const list = Array.isArray(ids) ? ids : [];
    if (list.length > MAX_BULK_ITEMS) {
      throw new Error(`db:deletePosts: too many ids (${list.length} > ${MAX_BULK_ITEMS}).`);
    }
    const { deleted, errors } = await removePostsAndFiles(list);
    return { ok: true, deleted, errors };
  });
  ipcMain.handle('db:bulkUpsert', (_, { posts, platform }) => {
    // The scraper injects MAIN-world scripts and posts 'intercepted' payloads, so
    // treat this array as untrusted: require an array, cap its size, and drop any
    // non-object entries before they reach the DB layer.
    if (!Array.isArray(posts)) return { inserted: 0, skipped: 0 };
    if (posts.length > MAX_BULK_ITEMS) {
      throw new Error(`bulkUpsert: too many posts (${posts.length} > ${MAX_BULK_ITEMS}).`);
    }
    const clean = posts.filter((p) => p && typeof p === 'object');
    const { inserted, skipped } = db.bulkUpsert(clean);
    if (inserted > 0) {
      sendToWindow('interceptor:newPosts', { count: inserted, platform });
    }
    return { inserted, skipped };
  });

  // ── Custom sources (collections) ──────────────────────────────────────────────

  ipcMain.handle('collections:list', () => db.getCollections());
  ipcMain.handle('collections:create', (_, { name, color, platform, externalId, igName }) =>
    db.createCollection({ name, color, platform, externalId, igName }),
  );
  ipcMain.handle('collections:update', (_, { id, name, color }) =>
    db.updateCollection(id, { name, color }),
  );
  // Delete a collection. By default only the tag is removed (posts stay in the
  // library, just un-filed — post_collections rows cascade away). With
  // deletePosts=true, every post currently in the collection is permanently
  // removed first, along with its on-disk files.
  ipcMain.handle('collections:delete', async (_, { id, deletePosts }) => {
    let deletedPosts = 0;
    const errors = [];
    if (deletePosts) {
      const r = await removePostsAndFiles(db.getPostIds({ collectionId: id }));
      deletedPosts = r.deleted;
      errors.push(...r.errors);
    }
    db.deleteCollection(id);
    return { ok: true, deletedPosts, errors };
  });
  ipcMain.handle('collections:addPosts', (_, { postIds, collectionIds }) => {
    // Validate + cap both renderer-supplied arrays before forwarding to the DB.
    if (!Array.isArray(postIds) || !Array.isArray(collectionIds)) {
      return { added: 0 };
    }
    if (postIds.length > MAX_BULK_ITEMS || collectionIds.length > MAX_BULK_ITEMS) {
      throw new Error('collections:addPosts: too many ids.');
    }
    return db.addPostsToCollections(postIds, collectionIds);
  });
  ipcMain.handle('collections:removePost', (_, { postId, collectionId }) =>
    db.removePostFromCollection(postId, collectionId),
  );

  // ── Downloads ───────────────────────────────────────────────────────────────

  ipcMain.handle('download:post', (_, { postId, assetTypes }) => {
    const post = db.getPost(postId);
    if (!post) throw new Error('Post not found: ' + postId);
    // Return the real count so the renderer can tell "nothing to download" (a web
    // reference / text-only post → 0) apart from a real, in-progress download.
    const queued = downloader.enqueuePost(post, assetTypes || ['thumbnail', 'image', 'video']);
    return { queued };
  });

  ipcMain.handle('download:all', async (_, { assetTypes, missingOnly } = {}) => {
    // For "missing" we fetch every post and let the queue skip asset types that
    // are already on disk — this catches partially-downloaded posts, not just
    // posts with no local asset at all.
    //
    // getPostIds() has no row cap (the old getPosts({ limit: 10000 }) silently
    // truncated libraries >10k and materialized everything synchronously). We
    // hydrate + enqueue in batches, yielding to the event loop between batches
    // so the main process stays responsive on large libraries.
    const ids = db.getPostIds();
    const types = assetTypes || ['thumbnail', 'image', 'video'];
    let queued = 0;
    for (let i = 0; i < ids.length; i += BATCH_SIZE) {
      const posts = db.getPostsByIds(ids.slice(i, i + BATCH_SIZE));
      const res = downloader.enqueueMany(posts, types, { missingOnly: !!missingOnly });
      queued += res?.queued ?? posts.length;
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { queued };
  });

  // Bulk download of an explicit id set (the gallery's multi-select). Mirrors
  // download:all — hydrate + enqueue in batches, yielding between them — but
  // scoped to the selected ids, and defaults to missingOnly so already-archived
  // assets aren't re-fetched.
  ipcMain.handle('download:posts', async (_, { ids, assetTypes, missingOnly = true } = {}) => {
    const list = Array.isArray(ids) ? ids : [];
    if (list.length > MAX_BULK_ITEMS) {
      throw new Error(`download:posts: too many ids (${list.length} > ${MAX_BULK_ITEMS}).`);
    }
    const types = assetTypes || ['thumbnail', 'image', 'video'];
    let queued = 0;
    for (let i = 0; i < list.length; i += BATCH_SIZE) {
      const posts = db.getPostsByIds(list.slice(i, i + BATCH_SIZE));
      const res = downloader.enqueueMany(posts, types, { missingOnly: !!missingOnly });
      queued += res?.queued ?? posts.length;
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { queued };
  });

  ipcMain.handle('download:status', () => downloader.getJobs());
  ipcMain.handle('download:isPaused', () => downloader.getIsPaused());

  ipcMain.handle('download:pauseAll', () => downloader.pauseAll());
  ipcMain.handle('download:resumeAll', () => downloader.resumeAll());
  ipcMain.handle('download:cancelAll', () => downloader.cancelAll());
  ipcMain.handle('download:clearCompleted', () => downloader.clearCompleted());

  ipcMain.handle('download:cancelJob', (_, { key }) => downloader.cancelJob(key));
  ipcMain.handle('download:retryJob', (_, { key }) => downloader.retryJob(key));

  // ── Analyze (local VLM categorization) ────────────────────────────────────────

  ipcMain.handle('analyze:post', (_, { postId }) => {
    const post = db.getPost(postId);
    if (!post) throw new Error('Post not found: ' + postId);
    return analyzer.enqueuePost(post);
  });

  // Mirror download:all — fetch ids first, then hydrate + enqueue in batches,
  // yielding to the event loop between batches so materializing the whole library
  // (light posts + attachMedia) and the per-post enqueue loop don't block the
  // main process on large libraries.
  ipcMain.handle('analyze:all', async () => {
    const ids = db.getPostIds();
    let queued = 0;
    for (let i = 0; i < ids.length; i += BATCH_SIZE) {
      const posts = db.getPostsForAnalysis({ ids: ids.slice(i, i + BATCH_SIZE) });
      queued += analyzer.enqueueMany(posts).queued;
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { queued };
  });

  ipcMain.handle('analyze:posts', async (_, { postIds }) => {
    const ids = Array.isArray(postIds) ? postIds : [];
    let queued = 0;
    for (let i = 0; i < ids.length; i += BATCH_SIZE) {
      const posts = db.getPostsForAnalysis({ ids: ids.slice(i, i + BATCH_SIZE) });
      queued += analyzer.enqueueMany(posts).queued;
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { queued };
  });

  // Split a selection ahead of a bulk analyze. The local VLM reads media off disk,
  // so a post whose media hasn't been downloaded can't be analyzed visually yet.
  // Partition the ids into the ones we can enqueue now — media already on disk, or
  // text-only bookmarks with nothing to download (the analyzer's caption fallback)
  // — and the ones that must be downloaded first (remote-only media). The gallery
  // enqueues `analyzable` and offers to download `needsDownload`. Column-based,
  // mirroring the hasLocalAsset/noLocalAsset semantics in db.buildPostFilter.
  const hasLocalVisualAsset = (p) =>
    !!(p.videoPath || p.imagePath || p.thumbnailPath) ||
    (Array.isArray(p.media) && p.media.some((m) => m && m.localPath));
  const hasDownloadableMedia = (p) =>
    !!p.thumbnailUrl ||
    (Array.isArray(p.media) && p.media.some((m) => m && m.url)) ||
    p.mediaType === 'image' ||
    p.mediaType === 'video';

  ipcMain.handle('analyze:split', async (_, { postIds } = {}) => {
    const ids = Array.isArray(postIds) ? postIds : [];
    const analyzable = [];
    const needsDownload = [];
    for (let i = 0; i < ids.length; i += BATCH_SIZE) {
      const posts = db.getPostsForAnalysis({ ids: ids.slice(i, i + BATCH_SIZE) });
      for (const p of posts) {
        if (hasLocalVisualAsset(p)) analyzable.push(p.id);
        else if (hasDownloadableMedia(p)) needsDownload.push(p.id);
        else analyzable.push(p.id); // text-only: nothing to download, analyze from caption
      }
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { analyzable, needsDownload };
  });

  ipcMain.handle('analyze:taxonomy', () => analyzer.getTaxonomy());

  ipcMain.handle('analyze:status', () => analyzer.getJobs());
  ipcMain.handle('analyze:cancelJob', (_, { key }) => analyzer.cancelJob(key));
  ipcMain.handle('analyze:cancelAll', () => analyzer.cancelAll());
  ipcMain.handle('analyze:clearAll', () => analyzer.clearAll());
  ipcMain.handle('analyze:clearCompleted', () => analyzer.clearCompleted());
  ipcMain.handle('analyze:pauseAll', () => analyzer.pauseAll());
  ipcMain.handle('analyze:resumeAll', () => analyzer.resumeAll());
  ipcMain.handle('analyze:isPaused', () => analyzer.getIsPaused());
  ipcMain.handle('analyze:retryJob', (_, { key }) => analyzer.retryJob(key));

  ipcMain.handle('analyze:modelStatus', () => analyzer.getModelStatus());
  ipcMain.handle('analyze:listModels', () => analyzer.listModels());
  ipcMain.handle('analyze:setModel', (_, { id }) => analyzer.setModel(id));
  ipcMain.handle('analyze:getConcurrency', () => ({
    value: analyzer.getConcurrency(),
    max: analyzer.MAX_CONCURRENCY,
  }));
  ipcMain.handle('analyze:setConcurrency', (_, { n }) => analyzer.setConcurrency(n));
  ipcMain.handle('analyze:getHardware', () => analyzer.getHardwareInfo());
  ipcMain.handle('analyze:getTuning', () => analyzer.getTuning());
  ipcMain.handle('analyze:setTuning', (_, { patch } = {}) => analyzer.setTuning(patch || {}));
  ipcMain.handle('analyze:downloadModel', (_, { id } = {}) =>
    analyzer.downloadModel(id, (progress, label) => {
      sendToWindow('analyze:modelProgress', { id, progress, label });
    }),
  );
  ipcMain.handle('analyze:pauseDownload', () => analyzer.pauseModelDownload());
  ipcMain.handle('analyze:cancelDownload', (_, { id } = {}) => analyzer.cancelModelDownload(id));
  ipcMain.handle('analyze:deleteModel', (_, { id }) => analyzer.deleteModel(id));

  // ── Web references (capture orchestration) ────────────────────────────────────
  // Mirror of download:*/analyze:*; the renderer reuses the same hook pattern.

  // Accepts a pasted URL, creates the placeholder + queues the pipeline. Returns
  // { id, finalUrl, domain, queued } early so the modal can close immediately.
  ipcMain.handle('web:add', (_, { url, maxPages, overwrite, singlePage } = {}) =>
    weborchestrator.enqueueWeb(url, { maxPages, overwrite, singlePage }),
  );
  ipcMain.handle('web:status', () => weborchestrator.getJobs());
  ipcMain.handle('web:isPaused', () => weborchestrator.getIsPaused());
  ipcMain.handle('web:cancel', (_, { key } = {}) => weborchestrator.cancelJob(key));
  ipcMain.handle('web:cancelAll', () => weborchestrator.cancelAll());
  ipcMain.handle('web:pauseAll', () => weborchestrator.pauseAll());
  ipcMain.handle('web:resumeAll', () => weborchestrator.resumeAll());
  ipcMain.handle('web:retryJob', (_, { key } = {}) => weborchestrator.retryJob(key));
  ipcMain.handle('web:clearCompleted', () => weborchestrator.clearCompleted());

  // Manual bookmark: user-added local files (images/videos/pdf/any) + note + tags.
  // Persisted as a platform='manual' post; the gallery picks it up via the same
  // new-posts event web/social use.
  ipcMain.handle('bookmark:add', async (_, payload = {}) => {
    // Defensive size guard. The files come from the renderer's File API (no
    // disk path to fs.stat), so the raw bytes necessarily travel in this one
    // invoke; the *primary* defense is the preload pre-flight, which rejects
    // oversized payloads in the renderer process before they are ever
    // structured-cloned into main. This re-check is the earliest point main
    // can act, and runs before any extra copy (bookmarks.toBuffer's
    // Buffer.from) or disk write. Caps mirror the renderer contract:
    // 200MB/file, 500MB total. Same throw style as bookmarks.addManualBookmark.
    //
    // Declarative fast-fail first: if an entry carries a declared `size`
    // (cheap scalar), reject on it immediately without touching the byte
    // buffers; the real byteLength is verified right after regardless, so a
    // lying `size` can't smuggle anything through.
    const byteLen = (v) => (v ? Number(v.byteLength ?? v.length) || 0 : 0);
    const files = Array.isArray(payload.files) ? payload.files : [];
    let declaredTotal = 0;
    for (const f of files) {
      const declared = Number(f && f.size) || 0;
      if (declared > MAX_BOOKMARK_FILE_BYTES) throw new Error('too-large');
      declaredTotal += declared;
    }
    if (declaredTotal > MAX_BOOKMARK_BYTES) throw new Error('too-large');
    let totalBytes = 0;
    for (const f of files) {
      const bytes = byteLen(f && f.original) + byteLen(f && f.preview);
      if (bytes > MAX_BOOKMARK_FILE_BYTES) throw new Error('too-large');
      totalBytes += bytes;
    }
    if (totalBytes > MAX_BOOKMARK_BYTES) throw new Error('too-large');
    const res = await bookmarks.addManualBookmark(payload);
    sendToWindow('interceptor:newPosts', { count: 1, platform: 'manual' });
    return res;
  });
  // Standalone page discovery (preview), optional for the UI.
  ipcMain.handle('web:discover', (_, { url, maxPages } = {}) =>
    weborchestrator.discover(url, { maxPages }),
  );

  // ── Web snapshots (dated version history) ─────────────────────────────────────
  // Archived (older) versions of a site; the current capture lives on the posts row.
  ipcMain.handle('web:getSnapshots', (_, { postId } = {}) => db.getWebSnapshots(postId));
  ipcMain.handle('web:snapshotCounts', () => db.getWebSnapshotCounts());

  // Full delete of one or more sites: abort any running capture, unlink every file
  // (current + all snapshots), then drop the rows (cascades to snapshots/media/…).
  ipcMain.handle('web:deleteSites', (_, { ids } = {}) => {
    const list = Array.isArray(ids) ? ids.filter(Boolean) : [];
    const errors = [];
    for (const id of list) {
      try {
        weborchestrator.cancelJob(`web:${id}`);
      } catch {}
      for (const p of db.getWebSiteFilePaths(id)) {
        try {
          fs.unlinkSync(p);
        } catch (e) {
          if (e.code !== 'ENOENT') errors.push(e.message);
        }
      }
    }
    const { deleted } = db.deletePosts(list);
    return { ok: true, deleted, errors };
  });

  // Delete one archived snapshot (from the version selector) + its files.
  ipcMain.handle('web:deleteSnapshot', (_, { id } = {}) => {
    const { pagePaths } = db.deleteWebSnapshot(id);
    const errors = [];
    for (const p of pagePaths) {
      try {
        fs.unlinkSync(p);
      } catch (e) {
        if (e.code !== 'ENOENT') errors.push(e.message);
      }
    }
    return { ok: true, errors };
  });

  // "Delete only the report": drop each site's current capture and promote its
  // most recent archived version (or reset to a placeholder when none remains),
  // unlinking the removed capture's files.
  ipcMain.handle('web:deleteLatestReport', (_, { ids } = {}) => {
    const list = Array.isArray(ids) ? ids.filter(Boolean) : [];
    const errors = [];
    let promoted = 0;
    let cleared = 0;
    for (const id of list) {
      const res = db.deleteLatestReport(id);
      if (res.promoted) promoted += 1;
      else cleared += 1;
      for (const p of res.removedPaths) {
        try {
          fs.unlinkSync(p);
        } catch (e) {
          if (e.code !== 'ENOENT') errors.push(e.message);
        }
      }
    }
    return { ok: true, promoted, cleared, errors };
  });

  // ── App / Updates ─────────────────────────────────────────────────────────────

  ipcMain.handle('app:getVersion', () => app.getVersion());
  ipcMain.handle('app:getUpdateChannel', () => updater.getUpdateChannel());
  ipcMain.handle('app:setUpdateChannel', (_, { channel } = {}) => {
    const ch = updater.setUpdateChannel(channel);
    updater.checkNow(_window);
    return ch;
  });
  ipcMain.handle('updater:getState', () => updater.getUpdateState());
  ipcMain.handle('updater:check', () => {
    updater.checkNow(_window);
    return updater.getUpdateState();
  });
  ipcMain.handle('updater:quitAndInstall', () => updater.quitAndInstall());
  ipcMain.handle('updater:openDownload', () => updater.openDownload());
  ipcMain.handle('updater:rebuild', () => updater.rebuildNow());

  // Runtime sidecar binaries (downloaded once into userData, not bundled)
  ipcMain.handle('binaries:status', () => binaries.status());
  ipcMain.handle('binaries:getVariant', () => binaries.getLlamaVariant());
  ipcMain.handle('binaries:setVariant', (_, { variant } = {}) => binaries.setLlamaVariant(variant));
  ipcMain.handle('binaries:variantState', () => binaries.getVariantState());
  ipcMain.handle('binaries:ensure', async (_, { force = false } = {}) => {
    try {
      const r = await binaries.ensureBinaries(
        (phase, fraction) => sendToWindow('binaries:progress', { phase, fraction }),
        { force },
      );
      sendToWindow('binaries:progress', { phase: 'done', fraction: 1 });
      return { ok: true, ...r };
    } catch (err) {
      sendToWindow('binaries:progress', { phase: 'error', error: String(err?.message || err) });
      return { ok: false, error: String(err?.message || err) };
    }
  });

  // ── AI Tags ───────────────────────────────────────────────────────────────────

  ipcMain.handle('aitags:overview', () => db.getAiOverview());
  ipcMain.handle('aitags:tagStats', (_, args) => db.getTagStats(args));
  ipcMain.handle('aitags:entityStats', (_, args) => db.getEntityStats(args));
  ipcMain.handle('aitags:cooccurrence', (_, { tag, limit }) =>
    db.getTagCooccurrence(tag, { limit }),
  );
  ipcMain.handle('aitags:clusters', (_, { maxClusters } = {}) => {
    // Sanitize the renderer-supplied cap: positive integer, hard-capped at 200.
    const n = Math.floor(Number(maxClusters));
    return db.getTagClusters(Number.isFinite(n) && n > 0 ? { maxClusters: Math.min(n, 200) } : {});
  });

  // Regenerate clusters: hybrid co-occurrence candidates refined by the local
  // model. Long-running and serial; progress is streamed to the renderer and the
  // run can be cancelled mid-flight via aitags:cluster:cancel.
  ipcMain.handle('aitags:cluster:regenerate', () => {
    if (_clusterAbort) _clusterAbort.abort();
    const ac = new AbortController();
    _clusterAbort = ac;
    return analyzer
      .clusterTags({
        signal: ac.signal,
        onProgress: (p) => {
          sendToWindow('aitags:clusterProgress', p);
        },
      })
      .finally(() => {
        if (_clusterAbort === ac) _clusterAbort = null;
      });
  });
  ipcMain.handle('aitags:cluster:cancel', () => {
    if (_clusterAbort) _clusterAbort.abort();
    return { cancelled: true };
  });

  // Genera PROPOSTE di alias (P3, canonicalizzazione sinonimi): il modello locale
  // mappa i quasi-sinonimi a una canonica esistente; le coppie vengono persistite
  // in tag_alias come 'proposed' (NON applicate a post_tags). Solo dopo accept/
  // dismiss l'utente decide se canonicalizzare. Orchestra analyzer.buildTagAliases
  // → db.saveTagAliases({ status: 'proposed' }) (l'analyzer NON scrive).
  ipcMain.handle('aitags:aliases:propose', async () => {
    if (_aliasAbort) _aliasAbort.abort();
    const ac = new AbortController();
    _aliasAbort = ac;
    try {
      const pairs = await analyzer.buildTagAliases({
        signal: ac.signal,
        onProgress: (p) => {
          sendToWindow('aitags:aliasProgress', p);
        },
      });
      db.saveTagAliases(pairs || [], { status: 'proposed' });
      return { ok: true, proposed: (pairs || []).length };
    } finally {
      if (_aliasAbort === ac) _aliasAbort = null;
    }
  });
  ipcMain.handle('aitags:aliases:cancel', () => {
    if (_aliasAbort) _aliasAbort.abort();
    return { cancelled: true };
  });
  // Review accept/reject delle proposte di alias (specchio dei cluster).
  ipcMain.handle('aitags:aliases:list', (_, { status } = {}) => db.getTagAliases({ status }));
  ipcMain.handle('aitags:alias:accept', (_, { aliasNorm }) =>
    db.setAliasStatus(aliasNorm, 'accepted'),
  );
  ipcMain.handle('aitags:alias:dismiss', (_, { aliasNorm }) =>
    db.setAliasStatus(aliasNorm, 'dismissed'),
  );
  ipcMain.handle('aitags:cluster:setStatus', (_, { id, status }) =>
    db.setClusterStatus(id, status),
  );
  ipcMain.handle('aitags:cluster:rename', (_, { id, label }) => db.renameCluster(id, label));
  ipcMain.handle('aitags:cluster:removeTag', (_, { tag, clusterId }) =>
    db.removeTagFromCluster(tag, clusterId),
  );
  ipcMain.handle('aitags:mergeSuggestions', (_, args) => db.getTagMergeSuggestions(args));
  ipcMain.handle('aitags:health', () => db.getTagHealth());
  ipcMain.handle('aitags:renameTag', (_, { from, to }) => db.renameTag(from, to));
  ipcMain.handle('aitags:mergeTags', (_, { sources, target }) => db.mergeTags(sources, target));
  ipcMain.handle('aitags:postIdsByTags', (_, { tags, mode }) => {
    if (!Array.isArray(tags)) return [];
    if (tags.length > MAX_BULK_ITEMS) {
      throw new Error(`aitags:postIdsByTags: too many tags (${tags.length} > ${MAX_BULK_ITEMS}).`);
    }
    return db.getPostIdsByTags(tags, mode);
  });
  ipcMain.handle('aitags:tagGraph', (_, args) => db.getTagGraph(args));

  ipcMain.handle('analyze:missing', async () => {
    // The missingOnly filter is applied at the DB level (no id-only query exists
    // for it), so we still materialize the filtered light-post list in one shot;
    // but we enqueue it in batches, yielding between them, so the per-post
    // enqueue loop doesn't block the main process on large libraries.
    const posts = db.getPostsForAnalysis({ missingOnly: true });
    let queued = 0;
    for (let i = 0; i < posts.length; i += BATCH_SIZE) {
      queued += analyzer.enqueueMany(posts.slice(i, i + BATCH_SIZE)).queued;
      await new Promise((resolve) => setImmediate(resolve));
    }
    return { queued };
  });

  // Manual edit of a post's AI analysis: mark it done and attributed to the user.
  ipcMain.handle('analyze:updateManual', (_, { id, fields }) =>
    db.updateAiAnalysis(id, { ...fields, status: 'done', model: 'manuale' }),
  );

  // User-authored layer (personal note + manual tags), kept distinct from the AI
  // fields so it survives an analysis regeneration. `fields` may carry { note?,
  // manualTags? } — only the keys present are written.
  ipcMain.handle('post:updateUserContent', (_, { id, fields }) => db.updateUserContent(id, fields));

  // Delete the AI-generated description for one or more posts (tags/status kept).
  ipcMain.handle('analyze:clearDescriptions', (_, { ids }) => db.clearAiDescriptions(ids));
  ipcMain.handle('analyze:clearTags', (_, { ids }) => db.clearAiTags(ids));

  // ── AI-assisted search ────────────────────────────────────────────────────────

  // Turn a free-text query into related filter tags via the local model. The
  // renderer shows these as clickable filter chips below the search bar. On
  // model-not-ready / any error: no suggestion (empty), so search degrades
  // gracefully.
  ipcMain.handle('search:suggest', async (_, { query }) => {
    const q = String(query || '').trim();
    if (!q) return { tags: [] };
    try {
      const { tags = [] } = await analyzer.expandSearchQuery(q);
      return { tags };
    } catch {
      return { tags: [] };
    }
  });

  // ── AI ▸ Search (conversational chat + tag/text search) ─────────────────────────

  // Multi-turn chat that streams the conversational reply over 'search:chatToken'
  // and returns the proposed tags. Only one chat runs at a time: starting a new
  // one aborts the previous in-flight request.
  ipcMain.handle('search:chat', async (_, { messages, activeTags } = {}) => {
    if (_chatAbort) _chatAbort.abort();
    const ac = new AbortController();
    _chatAbort = ac;
    const runId = ++_chatRunSeq;
    // Announce the run on the token channel BEFORE any token. IPC delivery is
    // FIFO, so the renderer adopts this runId first and can then discard any
    // straggler tokens still stamped with a superseded run's id.
    sendToWindow('search:chatToken', { start: true, runId });
    const onToken = (token) => sendToWindow('search:chatToken', { token, runId });
    try {
      return await analyzer.chatSearch(messages || [], activeTags || [], onToken, ac.signal);
    } finally {
      if (_chatAbort === ac) _chatAbort = null;
    }
  });

  // Aborts the currently-running chat (if any).
  ipcMain.handle('search:chatCancel', () => {
    if (_chatAbort) _chatAbort.abort();
    return { ok: true };
  });

  // Ranked tag search (matched-tag count DESC, then timestamp DESC). `source`
  // ('all'|'web'|'social') scopes results to web references / social posts.
  ipcMain.handle('search:byTags', (_, { tags, mode, limit, offset, source } = {}) =>
    db.searchPostsByTags(tags || [], { mode, limit, offset, source }),
  );

  // Hybrid: tag-filtered results merged with full-text matches on post content.
  ipcMain.handle('search:hybrid', (_, { tags, textQuery, mode, limit, offset, source } = {}) =>
    db.searchPostsHybrid(tags || [], textQuery || '', { mode, limit, offset, source }),
  );

  // Free-text search (LIKE over text/author/shortcode/ai_description/ai_tags).
  ipcMain.handle('search:byText', (_, { query, limit, offset, source } = {}) =>
    db.getPosts({ search: query, limit, offset, source }),
  );

  // ── Speech-to-text (local whisper.cpp) ───────────────────────────────────────

  ipcMain.handle('stt:status', () => stt.getStatus());
  ipcMain.handle('stt:listModels', () => stt.listModels());
  ipcMain.handle('stt:setModel', (_, { id }) => stt.setModel(id));
  ipcMain.handle('stt:downloadModel', (_, { id } = {}) =>
    stt.downloadModel(id, (progress, label) =>
      sendToWindow('stt:modelProgress', { id, progress, label }),
    ),
  );
  ipcMain.handle('stt:pauseDownload', () => stt.pauseModelDownload());
  ipcMain.handle('stt:cancelDownload', (_, { id } = {}) => stt.cancelModelDownload(id));
  ipcMain.handle('stt:deleteModel', (_, { id }) => stt.deleteModel(id));
  ipcMain.handle('stt:ensure', () => stt.ensureServer().then(() => ({ ok: true })));
  ipcMain.handle('stt:getTuning', () => stt.getTuning());
  ipcMain.handle('stt:setTuning', (_, { patch } = {}) => stt.setTuning(patch || {}));

  // ── Embeddings (clustering semantico locale, P4) ──────────────────────────────
  // Modello di embedding opzionale: se non scaricato, il clustering ricade su
  // sola co-occorrenza. Stesso pattern di download/lifecycle di stt:*.
  ipcMain.handle('emb:status', () => embeddings.getStatus());
  ipcMain.handle('emb:listModels', () => embeddings.listModels());
  ipcMain.handle('emb:setModel', (_, { id }) => embeddings.setModel(id));
  ipcMain.handle('emb:downloadModel', (_, { id } = {}) =>
    embeddings.downloadModel(id, (progress, label) =>
      sendToWindow('emb:modelProgress', { id, progress, label }),
    ),
  );
  ipcMain.handle('emb:pauseDownload', () => embeddings.pauseModelDownload());
  ipcMain.handle('emb:cancelDownload', (_, { id } = {}) => embeddings.cancelModelDownload(id));
  ipcMain.handle('emb:deleteModel', (_, { id }) => embeddings.deleteModel(id));
  ipcMain.handle('stt:transcribe', (_, { wav, language } = {}) => {
    // Validate the audio payload before it reaches Buffer.from, which would
    // otherwise throw a raw TypeError on a bad type. Cap the size to guard
    // against a runaway renderer handing us an enormous buffer.
    const MAX_WAV_BYTES = 50 * 1024 * 1024; // ~50MB
    const isArrayBuffer = wav instanceof ArrayBuffer;
    const isView = ArrayBuffer.isView(wav);
    if (!isArrayBuffer && !isView) {
      return { error: 'Invalid audio payload: expected an ArrayBuffer or typed array.' };
    }
    if (wav.byteLength > MAX_WAV_BYTES) {
      return {
        error: `Audio too large: ${wav.byteLength} bytes exceeds the ${MAX_WAV_BYTES}-byte limit.`,
      };
    }
    return stt.transcribe(wav, { language });
  });

  // ── Dialog / shell ──────────────────────────────────────────────────────────

  // Provide the MAIN-world fetch/XHR patch to the renderer on demand. Reading it
  // here (cached) keeps fs out of the preload; the renderer injects it into the
  // webview's MAIN world via executeJavaScript.
  ipcMain.handle('getWebviewInjectedScript', () => getWebviewInjectedScript());
  ipcMain.handle('getWebviewSelectScript', () => getWebviewSelectScript());

  ipcMain.handle('dialog:openFile', async () => {
    const result = await dialog.showOpenDialog(
      _window && !_window.isDestroyed() ? _window : undefined,
      {
        properties: ['openFile'],
        filters: [{ name: 'JSON', extensions: ['json'] }],
      },
    );
    if (result.canceled || !result.filePaths[0]) return null;
    const picked = result.filePaths[0];
    // Remember only the most recent choice so db:importJSON can validate the path
    // came from this dialog rather than being supplied arbitrarily by the renderer.
    _lastPickedImportPath = picked;
    return picked;
  });

  ipcMain.handle('shell:openPath', (_, { path: filePath }) => {
    // Local assets only: confine to the app's userData tree, reject anything else.
    const resolved = confineToUserData(filePath);
    if (!resolved) return { ok: false };
    return shell.openPath(resolved);
  });
  ipcMain.handle('shell:showItemInFolder', (_, { path: filePath }) => {
    const resolved = confineToUserData(filePath);
    if (!resolved) return { ok: false };
    return shell.showItemInFolder(resolved);
  });
  ipcMain.handle('shell:openExternal', (_, { url }) => {
    // Run through the shared net-safety policy for consistency with web:add and
    // the downloader: http(s) only, and reject loopback/link-local/private/
    // metadata hosts (assertSafeUrl throws on any of these, and on bad schemes).
    try {
      netSafety.assertSafeUrl(url);
    } catch {
      return { ok: false };
    }
    return shell.openExternal(url);
  });

  // ── Feedback (email allo sviluppatore via Resend, dal main process) ──────────
  ipcMain.handle('feedback:send', (_, payload = {}) =>
    feedback.sendFeedback({
      message: payload.message,
      attachments: payload.attachments,
      version: app.getVersion(),
    }),
  );
}

module.exports = { registerIpcHandlers };
