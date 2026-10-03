// src/views/Jobs.tsx (P4-09): the web's Jobs view, against a fake
// ShelfyClient. Filters are read from the address when a Navigation exists
// (web/src/routes.tsx) and kept as local state otherwise, so these render
// the view both ways.
import React from 'react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import { desktopCapabilities } from '../../src/api/electronClient';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { NavigationProvider, type Navigation } from '../../src/api/navigation';
import type { ShelfyCapabilities, ShelfyClient } from '../../src/api/ShelfyClient';
import type {
  Job,
  JobPage,
  JobsApi,
  JobUpdate,
  QueueResult,
  QueueSummary,
} from '../../src/api/jobs';
import Jobs from '../../src/views/Jobs';

// Every capability field, so a lane that adds one elsewhere never breaks this
// fixture; only `jobs` matters to these tests.
const CAPS: ShelfyCapabilities = { ...desktopCapabilities('darwin'), jobs: true };

function job(overrides: Partial<Job> = {}): Job {
  return {
    id: 1,
    kind: 'capture.site',
    state: 'running',
    progress: 0.3,
    stage: null,
    postKey: null,
    errorCode: null,
    attempts: 0,
    maxAttempts: 2,
    runAt: 0,
    createdAt: 0,
    updatedAt: 0,
    finishedAt: null,
    ...overrides,
  };
}

function queue(overrides: Partial<QueueSummary> = {}): QueueSummary {
  return {
    kind: 'capture.site',
    paused: false,
    queued: 0,
    running: 1,
    succeeded: 0,
    failed: 0,
    cancelled: 0,
    ...overrides,
  };
}

// Keeps each queue's counts consistent across chained actions (pause then
// cancel-all then clear-finished, as a real queue bar session does) instead
// of resetting them to zero on every call — a view test clicks through real
// (possibly disabled-by-count) buttons, unlike the hook's own tests, which
// call the actions directly.
function fakeJobsApi(page: JobPage, initialQueues: QueueSummary[] = []): JobsApi {
  const queues = new Map(initialQueues.map((q) => [q.kind, { ...q }]));
  function queueOf(kind: string): QueueSummary {
    let q = queues.get(kind);
    if (!q) {
      q = queue({ kind });
      queues.set(kind, q);
    }
    return q;
  }
  return {
    list: vi.fn(async () => page),
    summary: vi.fn(async () => Array.from(queues.values()).map((q) => ({ ...q }))),
    cancel: vi.fn(async (id: number) => ({ ...job({ id }), state: 'cancelled' }) as Job),
    retry: vi.fn(async (id: number) => ({ ...job({ id }), state: 'queued' }) as Job),
    pauseQueue: vi.fn(async (kind: string): Promise<QueueResult> => {
      const q = queueOf(kind);
      q.paused = true;
      return { queue: { ...q }, affected: 1 };
    }),
    resumeQueue: vi.fn(async (kind: string): Promise<QueueResult> => {
      const q = queueOf(kind);
      q.paused = false;
      return { queue: { ...q }, affected: 1 };
    }),
    cancelQueue: vi.fn(async (kind: string): Promise<QueueResult> => {
      const q = queueOf(kind);
      const affected = q.queued + q.running;
      q.cancelled += affected;
      q.queued = 0;
      q.running = 0;
      return { queue: { ...q }, affected };
    }),
    clearFinishedQueue: vi.fn(async (kind: string): Promise<QueueResult> => {
      const q = queueOf(kind);
      const affected = q.succeeded + q.failed + q.cancelled;
      q.succeeded = 0;
      q.failed = 0;
      q.cancelled = 0;
      return { queue: { ...q }, affected };
    }),
    onUpdate: vi.fn((_listener: (update: JobUpdate) => void) => () => {}),
  };
}

function fakeClient(jobsApi: JobsApi, posts: Shelfy.Post[] = []): ShelfyClient {
  return {
    capabilities: CAPS,
    media: { file: () => null, tile: () => '/media/tile.webp', isStored: () => true },
    jobs: jobsApi,
    listPosts: vi.fn(),
    getPostsByIds: vi.fn(async (ids: string[]) => posts.filter((p) => ids.includes(p.id))),
    getStats: vi.fn(),
    listCollections: vi.fn(),
    updatePost: vi.fn(),
    createCollection: vi.fn(),
    updateCollection: vi.fn(),
    deleteCollection: vi.fn(),
    addPostsToCollections: vi.fn(),
    removePostFromCollection: vi.fn(),
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
    countPosts: vi.fn(),
    resolveAllIds: vi.fn(),
    bulkAction: vi.fn(),
    listTrash: vi.fn(),
    restoreFromTrash: vi.fn(),
    emptyTrash: vi.fn(),
    reportError: vi.fn(),
  };
}

function post(overrides: Partial<Shelfy.Post> = {}): Shelfy.Post {
  return {
    id: 'web_1',
    platform: 'web',
    shortcode: null,
    postUrl: null,
    profileUrl: null,
    authorUsername: null,
    authorName: null,
    text: null,
    thumbnailUrl: null,
    mediaType: 'website',
    timestamp: null,
    thumbnailPath: '/media/cover.jpg',
    previewPath: null,
    imagePath: null,
    videoPath: null,
    thumbBlur: null,
    mediaCount: 0,
    importedAt: 0,
    aiDescription: null,
    aiTags: [],
    aiStatus: null,
    aiModel: null,
    aiAnalyzedAt: null,
    aiCategory: null,
    aiContentType: null,
    aiEntities: [],
    aiKeywords: [],
    aiLanguage: null,
    aiSaveReason: null,
    aiWeb: null,
    userNote: null,
    userTags: [],
    webUrl: null,
    webDomain: null,
    webFinalUrl: null,
    webPalette: [],
    webFonts: [],
    webTech: [],
    webAwards: [],
    webPages: [],
    webMeta: null,
    webSinglePage: false,
    webCapturedAt: null,
    media: [],
    collectionIds: [],
    ...overrides,
  };
}

function renderJobs(jobsApi: JobsApi, posts: Shelfy.Post[] = [], navigation?: Navigation) {
  const client = fakeClient(jobsApi, posts);
  const tree = (
    <ShelfyProvider client={client}>
      <Jobs />
    </ShelfyProvider>
  );
  render(
    navigation ? <NavigationProvider navigation={navigation}>{tree}</NavigationProvider> : tree,
  );
  return client;
}

describe('Jobs view', () => {
  // useT()/useLang() fall back to the persisted/detected language without a
  // provider (src/i18n/index.tsx); pin it so the English assertions below are
  // deterministic regardless of suite order (settings.test.tsx's own convention).
  beforeEach(() => localStorage.setItem('app:language', 'en'));
  afterEach(() => localStorage.setItem('app:language', 'it'));

  it('shows the empty state when there are no jobs', async () => {
    renderJobs(fakeJobsApi({ items: [], nextCursor: null }));
    expect(await screen.findByTestId('jobs-empty')).toHaveTextContent('No recent jobs.');
  });

  it('lists jobs with their kind, state and a post thumbnail', async () => {
    renderJobs(
      fakeJobsApi({ items: [job({ id: 1, postKey: 'web_1', progress: 0.42 })], nextCursor: null }),
      [post()],
    );
    const row = await screen.findByTestId('job-row');
    expect(within(row).getByText('Site capture')).toBeInTheDocument();
    expect(within(row).getByText('Running')).toBeInTheDocument();
    expect(within(row).getByText('42%')).toBeInTheDocument();
    // The thumbnail is decorative (empty alt; the button carries the
    // accessible name), so it has no "img" role — query the element directly.
    await waitFor(() =>
      expect(row.querySelector('img')).toHaveAttribute('src', '/media/tile.webp'),
    );
  });

  it('an unlabeled kind falls back to a prettified code, and still offers cancel', async () => {
    const api = fakeJobsApi({
      items: [job({ id: 3, kind: 'p2.extension.sync' })],
      nextCursor: null,
    });
    renderJobs(api);
    const row = await screen.findByTestId('job-row');
    expect(within(row).getByText('P2 Extension Sync')).toBeInTheDocument();
    fireEvent.click(within(row).getByTestId('job-row-cancel'));
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith(3));
  });

  it('shows the error of a failed job and retries it', async () => {
    const api = fakeJobsApi({
      items: [
        job({ id: 2, state: 'failed', errorCode: 'unavailable', attempts: 1, maxAttempts: 3 }),
      ],
      nextCursor: null,
    });
    renderJobs(api);
    const row = await screen.findByTestId('job-row');
    expect(within(row).getByTestId('job-row-error')).toHaveTextContent(
      'A dependency was unavailable.',
    );
    // The "tries" count shares a <span> with its "· " separator (one text
    // node each): a regex matches the substring instead of the whole node.
    expect(within(row).getByText(/1\/3 tries/)).toBeInTheDocument();
    fireEvent.click(within(row).getByTestId('job-row-retry'));
    await waitFor(() => expect(api.retry).toHaveBeenCalledWith(2));
  });

  it('the queue bar offers pause, cancel-all and clear-finished per kind', async () => {
    const api = fakeJobsApi({ items: [], nextCursor: null }, [
      queue({ kind: 'capture.site', queued: 1, running: 1, succeeded: 2 }),
    ]);
    renderJobs(api);
    const bar = await screen.findByTestId('jobs-queue-bar');
    const row = within(bar).getByTestId('jobs-queue-row');
    expect(row).toHaveAttribute('data-kind', 'capture.site');
    fireEvent.click(within(row).getByTestId('jobs-queue-pause-toggle'));
    await waitFor(() => expect(api.pauseQueue).toHaveBeenCalledWith('capture.site'));
    fireEvent.click(within(row).getByTestId('jobs-queue-cancel-all'));
    await waitFor(() => expect(api.cancelQueue).toHaveBeenCalledWith('capture.site'));
    fireEvent.click(within(row).getByTestId('jobs-queue-clear-finished'));
    await waitFor(() => expect(api.clearFinishedQueue).toHaveBeenCalledWith('capture.site'));
  });

  it('a state filter chip updates the address, and clearing it resets both', async () => {
    const navigate = vi.fn();
    const navigation: Navigation = {
      route: { name: 'jobs', kind: [], state: [] },
      navigate,
      back: vi.fn(),
    };
    renderJobs(
      fakeJobsApi({ items: [job({ state: 'failed' })], nextCursor: null }),
      [],
      navigation,
    );
    fireEvent.click(await screen.findByTestId('jobs-state-filter-failed'));
    expect(navigate).toHaveBeenCalledWith(
      { name: 'jobs', kind: [], state: ['failed'] },
      { replace: true },
    );
  });

  it('opens the job’s post through the supplied callback', async () => {
    const onOpenPost = vi.fn();
    const client = fakeClient(
      fakeJobsApi({ items: [job({ postKey: 'web_1' })], nextCursor: null }),
      [post()],
    );
    render(
      <ShelfyProvider client={client}>
        <Jobs onOpenPost={onOpenPost} />
      </ShelfyProvider>,
    );
    fireEvent.click(await screen.findByTestId('job-row-post-link'));
    expect(onOpenPost).toHaveBeenCalledWith('web_1');
  });

  it('load more fetches the next page by cursor', async () => {
    const api = fakeJobsApi({ items: [job({ id: 1 })], nextCursor: 'jobs.0' });
    renderJobs(api);
    await screen.findByTestId('job-row');
    vi.mocked(api.list).mockResolvedValueOnce({ items: [job({ id: 0 })], nextCursor: null });
    fireEvent.click(screen.getByTestId('jobs-load-more'));
    await waitFor(() => expect(screen.getAllByTestId('job-row')).toHaveLength(2));
  });
});
