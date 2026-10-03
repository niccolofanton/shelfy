import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import Trash from '../../src/views/Trash';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { desktopCapabilities } from '../../src/api/electronClient';
import type { ShelfyClient, TrashPage, BulkOutcome } from '../../src/api/ShelfyClient';

const POSTS = [
  { id: 'p1', platform: 'instagram', authorUsername: 'a', mediaType: 'image', deletedAt: 300 },
  { id: 'p2', platform: 'twitter', authorUsername: 'b', mediaType: 'image', deletedAt: 200 },
] as unknown as Shelfy.Post[];

const outcome = (overrides: Partial<BulkOutcome> = {}): BulkOutcome => ({
  changed: 0,
  selected: 0,
  deletedAt: null,
  job: null,
  ...overrides,
});

function fakeClient(overrides: Partial<ShelfyClient> = {}): ShelfyClient {
  return {
    capabilities: desktopCapabilities('darwin'),
    media: { file: () => null, tile: () => null, isStored: () => false },
    listPosts: vi.fn(),
    getPostsByIds: vi.fn().mockResolvedValue([]),
    getStats: vi.fn(),
    listCollections: vi.fn().mockResolvedValue([]),
    countPosts: vi.fn().mockResolvedValue(0),
    resolveAllIds: vi.fn().mockResolvedValue(null),
    bulkAction: vi.fn().mockResolvedValue(outcome()),
    listTrash: vi.fn().mockResolvedValue({
      posts: [],
      total: 0,
      retentionDays: 30,
      nextCursor: null,
    } satisfies TrashPage),
    restoreFromTrash: vi.fn().mockResolvedValue(outcome()),
    emptyTrash: vi.fn().mockResolvedValue({ selected: 0, job: null }),
    updatePost: vi.fn(),
    createCollection: vi.fn(),
    updateCollection: vi.fn(),
    deleteCollection: vi.fn(),
    addPostsToCollections: vi.fn(),
    removePostFromCollection: vi.fn(),
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
    reportError: vi.fn(),
    ...overrides,
  };
}

function renderTrash(client: ShelfyClient) {
  return render(
    <ShelfyProvider client={client}>
      <Trash />
    </ShelfyProvider>,
  );
}

describe('Trash view — empty state', () => {
  it('shows the empty state when there is nothing in the trash', async () => {
    const client = fakeClient();
    renderTrash(client);
    expect(await screen.findByTestId('trash-empty-state')).toBeInTheDocument();
    expect(screen.getByTestId('trash-count')).toHaveTextContent('0');
    expect(screen.queryByTestId('trash-select-toggle')).toBeNull();
    // TR-1/TR-2: nothing to empty, and the retention line would only repeat
    // the empty state's body.
    expect(screen.queryByTestId('trash-empty')).toBeNull();
    expect(screen.getAllByText(/30 giorni/)).toHaveLength(1);
    // No search here at all (review L6: trashed posts have no FTS rows).
    expect(screen.queryByRole('textbox')).toBeNull();
  });

  it('surfaces a load error', async () => {
    const client = fakeClient({ listTrash: vi.fn().mockRejectedValue(new Error('boom')) });
    renderTrash(client);
    expect(await screen.findByTestId('trash-empty-state')).toHaveTextContent(
      'Caricamento del cestino non riuscito.',
    );
  });
});

describe('Trash view — list, select and restore', () => {
  function withPosts() {
    return fakeClient({
      listTrash: vi.fn().mockResolvedValue({
        posts: POSTS,
        total: 2,
        retentionDays: 30,
        nextCursor: null,
      } satisfies TrashPage),
    });
  }

  it('lists the trashed posts and the retention note', async () => {
    const client = withPosts();
    renderTrash(client);
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    expect(screen.getByTestId('trash-count')).toHaveTextContent('2');
    expect(screen.getByText(/30 giorni/)).toBeInTheDocument();
  });

  it('selects one post and restores it by key', async () => {
    const client = withPosts();
    vi.mocked(client.restoreFromTrash).mockResolvedValue(outcome({ changed: 1, selected: 1 }));
    renderTrash(client);
    const [card] = await screen.findAllByTestId('post-card');
    fireEvent.click(card);
    expect(screen.getByTestId('trash-selection-count')).toHaveTextContent('1');

    fireEvent.click(screen.getByTestId('trash-restore'));
    fireEvent.click(screen.getByTestId('trash-restore')); // two-step confirm
    await waitFor(() => expect(client.restoreFromTrash).toHaveBeenCalledWith({ keys: ['p1'] }));
    // Exits selection and reconciles the list.
    await waitFor(() => expect(client.listTrash).toHaveBeenCalledTimes(2));
  });

  it('"select all" picks every loaded post when the trash fits on one page', async () => {
    const client = withPosts();
    renderTrash(client);
    await screen.findAllByTestId('post-card');
    fireEvent.click(screen.getByTestId('trash-select-toggle'));
    fireEvent.click(screen.getByTestId('trash-select-all'));
    expect(screen.getByTestId('trash-selection-count')).toHaveTextContent('2');
    expect(client.resolveAllIds).not.toHaveBeenCalled();
  });

  it('select-all-matching restores by filter+exceptKeys when more trash exists than is loaded', async () => {
    const client = fakeClient({
      listTrash: vi.fn().mockResolvedValue({
        posts: POSTS,
        total: 500,
        retentionDays: 30,
        nextCursor: 'c1',
      } satisfies TrashPage),
    });
    vi.mocked(client.restoreFromTrash).mockResolvedValue(outcome({ changed: 498, selected: 498 }));
    renderTrash(client);
    await screen.findAllByTestId('post-card');
    fireEvent.click(screen.getByTestId('trash-select-toggle'));
    fireEvent.click(screen.getByTestId('trash-select-all'));
    expect(screen.getByTestId('trash-selection-count')).toHaveTextContent('500');

    fireEvent.click(screen.getByTestId('trash-restore'));
    fireEvent.click(screen.getByTestId('trash-restore'));
    await waitFor(() =>
      expect(client.restoreFromTrash).toHaveBeenCalledWith({
        filter: { trash: true },
        exceptKeys: [],
      }),
    );
  });

  it('shows job progress when a bulk restore is queued (>500 posts), then clears on completion', async () => {
    const client = withPosts();
    let jobListener: ((evt: unknown) => void) | undefined;
    vi.mocked(client.on).mockImplementation(((type: string, listener: (e: unknown) => void) => {
      if (type === 'job.updated') jobListener = listener;
      return () => {};
    }) as ShelfyClient['on']);
    vi.mocked(client.restoreFromTrash).mockResolvedValue(
      outcome({ changed: null, selected: 600, job: { id: 5, kind: 'bulk' } }),
    );
    renderTrash(client);
    const [card] = await screen.findAllByTestId('post-card');
    fireEvent.click(card);
    fireEvent.click(screen.getByTestId('trash-restore'));
    fireEvent.click(screen.getByTestId('trash-restore'));
    expect(await screen.findByTestId('trash-job-toast')).toBeInTheDocument();

    act(() => {
      jobListener?.({ id: 5, kind: 'bulk', state: 'succeeded', progress: 1, errorCode: null });
    });
    await waitFor(() => expect(screen.queryByTestId('trash-job-toast')).toBeNull());
  });
});

describe('Trash view — empty trash', () => {
  it('requires a two-step confirm, then starts the purge', async () => {
    const client = fakeClient({
      listTrash: vi.fn().mockResolvedValue({
        posts: POSTS,
        total: 2,
        retentionDays: 30,
        nextCursor: null,
      } satisfies TrashPage),
    });
    vi.mocked(client.emptyTrash).mockResolvedValue({ selected: 2, job: null });
    renderTrash(client);
    await screen.findAllByTestId('post-card');

    fireEvent.click(screen.getByTestId('trash-empty'));
    expect(client.emptyTrash).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('trash-empty-confirm'));
    await waitFor(() => expect(client.emptyTrash).toHaveBeenCalledTimes(1));
  });

  it('hides the button when the trash is already empty', async () => {
    const client = fakeClient();
    renderTrash(client);
    await screen.findByTestId('trash-empty-state');
    expect(screen.queryByTestId('trash-empty')).toBeNull();
  });
});
