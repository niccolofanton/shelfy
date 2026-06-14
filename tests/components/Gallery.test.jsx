import { render, screen, fireEvent, waitFor, within, act } from '@testing-library/react';
import Gallery from '../../src/views/Gallery';

// Wrap VirtualPostGrid to capture the latest props the Gallery passes it — the
// quick-select contract (onQuickSelect) is invoked by the card's hover checkbox,
// which belongs to the card redesign, so tests trigger it through the captured
// prop. The real grid + cards still render (every other test relies on them).
const gridProps = vi.hoisted(() => ({ current: null }));
vi.mock('../../src/components/VirtualPostGrid', async (importOriginal) => {
  const actual = await importOriginal();
  const { createElement } = await import('react');
  return {
    ...actual,
    default: function VirtualPostGridSpy(props) {
      gridProps.current = props;
      return createElement(actual.default, props);
    },
  };
});

const POSTS = [
  {
    id: 'p1',
    platform: 'instagram',
    authorUsername: 'a',
    mediaType: 'image',
    thumbnailUrl: 'u1',
    collectionIds: [],
  },
  {
    id: 'p2',
    platform: 'twitter',
    authorUsername: 'b',
    mediaType: 'image',
    thumbnailUrl: 'u2',
    collectionIds: [],
  },
];

const COLLECTIONS = [
  { id: 1, name: 'Ricette', color: '#e91e63', count: 0 },
  { id: 2, name: 'Viaggi', color: '#4caf50', count: 0 },
];

beforeEach(() => {
  window.electronAPI.getPosts.mockResolvedValue({ posts: POSTS, total: POSTS.length });
});

async function renderGallery(props = {}) {
  const utils = render(
    <Gallery
      collections={COLLECTIONS}
      onCreateCollection={vi.fn()}
      onAssigned={vi.fn()}
      {...props}
    />,
  );
  await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(POSTS.length));
  return utils;
}

describe('Gallery — selection mode', () => {
  it('enters selection mode and shows checkboxes instead of opening the modal', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));

    expect(screen.getAllByTestId('select-checkbox')).toHaveLength(POSTS.length);

    // Clicking a card selects it rather than opening the post modal.
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    expect(screen.queryByTestId('post-modal')).toBeNull();
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');
  });

  it('assigns the selected posts to a collection', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('assign-to-1'));

    await waitFor(() =>
      expect(window.electronAPI.addPostsToCollections).toHaveBeenCalledWith(['p1', 'p2'], [1]),
    );
  });

  it('lists the available collections in the actions menu', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getByTestId('bulk-actions'));

    const menu = screen.getByTestId('bulk-actions-menu');
    expect(within(menu).getByText('Ricette')).toBeInTheDocument();
    expect(within(menu).getByText('Viaggi')).toBeInTheDocument();
  });

  it('exits selection mode when the filters change (stale ids would linger)', async () => {
    await renderGallery();
    // Open the filters drawer while still in browse mode: once selection starts
    // the unified toolbar swaps to the action bar (no search / Filtri there),
    // but the already-open drawer keeps working.
    fireEvent.click(screen.getByTestId('filters-toggle'));
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');

    // Changing a real filter (media type) leaves selection mode entirely —
    // keeping it armed over a different result set is surprising.
    const mediaButtons = within(screen.getByTestId('drawer-mediatype')).getAllByRole('button');
    fireEvent.click(mediaButtons[1]);
    await waitFor(() => expect(screen.queryByTestId('selection-count')).toBeNull());
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
  });

  it('exits selection mode when the view goes inactive (tab change)', async () => {
    const { rerender } = await renderGallery({ active: true });
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');

    // The kept-alive view flips `active` off when another tab takes over.
    rerender(
      <Gallery
        collections={COLLECTIONS}
        onCreateCollection={vi.fn()}
        onAssigned={vi.fn()}
        active={false}
      />,
    );
    expect(screen.queryByTestId('selection-count')).toBeNull();
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
  });

  it('cancelling selection clears the selected posts', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getByTestId('select-cancel'));

    expect(screen.queryByTestId('select-checkbox')).toBeNull();
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
  });
});

describe('Gallery — bulk actions', () => {
  it('"Analizza" queues the selected ids via analyzePosts', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('bulk-analyze'));
    await waitFor(() => expect(window.electronAPI.analyzePosts).toHaveBeenCalledWith(['p1', 'p2']));
  });

  it('"Analizza" enqueues only the on-disk ids and suggests downloading the rest', async () => {
    window.electronAPI.splitForAnalysis.mockResolvedValue({
      analyzable: ['p1'],
      needsDownload: ['p2'],
    });
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('bulk-analyze'));

    // Only the downloaded post is enqueued for analysis…
    await waitFor(() => expect(window.electronAPI.analyzePosts).toHaveBeenCalledWith(['p1']));
    // …and the remote-only one is surfaced as an actionable download suggestion.
    expect(await screen.findByTestId('analyze-download-suggest')).toBeInTheDocument();
  });

  it('the analyze download suggestion enqueues exactly the remote-only ids', async () => {
    window.electronAPI.splitForAnalysis.mockResolvedValue({
      analyzable: ['p1'],
      needsDownload: ['p2'],
    });
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('bulk-analyze'));

    fireEvent.click(await screen.findByTestId('analyze-download-suggest-action'));
    await waitFor(() =>
      expect(window.electronAPI.downloadPosts).toHaveBeenCalledWith(
        ['p2'],
        expect.arrayContaining(['thumbnail', 'image', 'video']),
      ),
    );
    // The whole selection was never re-downloaded, only the missing ids.
    expect(window.electronAPI.downloadPosts).toHaveBeenCalledTimes(1);
  });

  it('"Analizza" with nothing on disk skips analysis and only suggests downloading', async () => {
    window.electronAPI.splitForAnalysis.mockResolvedValue({
      analyzable: [],
      needsDownload: ['p1', 'p2'],
    });
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('bulk-analyze'));

    expect(await screen.findByTestId('analyze-download-suggest')).toBeInTheDocument();
    expect(window.electronAPI.analyzePosts).not.toHaveBeenCalled();
  });

  it('"Scarica" enqueues the selected ids via downloadPosts', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    fireEvent.click(screen.getAllByTestId('post-card')[1]);

    fireEvent.click(screen.getByTestId('bulk-actions'));
    fireEvent.click(screen.getByTestId('bulk-download'));
    await waitFor(() =>
      expect(window.electronAPI.downloadPosts).toHaveBeenCalledWith(
        ['p1', 'p2'],
        expect.arrayContaining(['thumbnail', 'image', 'video']),
      ),
    );
  });

  it('"Seleziona tutti" appears when total > loaded and selects all matching ids', async () => {
    // total greater than the loaded posts triggers the "select all matching" CTA.
    window.electronAPI.getPosts.mockResolvedValue({ posts: POSTS, total: 200 });
    window.electronAPI.getPostIds.mockResolvedValue(['p1', 'p2', 'p3', 'p4']);

    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));

    // The toggle sits in the toolbar next to the counter, not in the actions menu.
    const selectAll = screen.getByTestId('select-all-matching');
    expect(selectAll).toHaveTextContent('200');
    fireEvent.click(selectAll);

    await waitFor(() => expect(window.electronAPI.getPostIds).toHaveBeenCalled());
    await waitFor(() =>
      expect(screen.getByTestId('selection-count')).toHaveTextContent('4 selezionati'),
    );
  });

  it('"Seleziona tutti" checks the loaded cards instantly, before the id query resolves', async () => {
    // Large library: the loaded ids select synchronously so the click feels
    // responsive, then the full matching set merges in once getPostIds returns.
    window.electronAPI.getPosts.mockResolvedValue({ posts: POSTS, total: 200 });
    let resolveIds;
    window.electronAPI.getPostIds.mockReturnValue(
      new Promise((resolve) => {
        resolveIds = resolve;
      }),
    );

    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getByTestId('select-all-matching'));

    // Instant: the 2 loaded cards are selected while getPostIds is still pending.
    expect(screen.getByTestId('selection-count')).toHaveTextContent('2 selezionati');

    resolveIds(['p1', 'p2', 'p3', 'p4']);
    await waitFor(() =>
      expect(screen.getByTestId('selection-count')).toHaveTextContent('4 selezionati'),
    );
  });

  it('shows "Seleziona tutti" even when everything is loaded and selects the loaded ids', async () => {
    // total === POSTS.length (e.g. a small IG/Pinterest folder): the select-all
    // affordance must still be present and pick every loaded post — no IPC needed.
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));

    const selectAll = screen.getByTestId('select-all-matching');
    expect(selectAll).toHaveTextContent('Seleziona tutti');
    fireEvent.click(selectAll);

    expect(window.electronAPI.getPostIds).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(screen.getByTestId('selection-count')).toHaveTextContent('2 selezionati'),
    );

    // Now that all matching posts are selected, the toggle flips to deselect.
    expect(screen.getByTestId('select-all-matching')).toHaveTextContent('Deseleziona tutti');
    fireEvent.click(screen.getByTestId('select-all-matching'));
    await waitFor(() =>
      expect(screen.getByTestId('selection-count')).toHaveTextContent('0 selezionati'),
    );
  });
});

describe('Gallery — drag-select', () => {
  it('sweeping the mouse across cards selects the range (and suppresses the trailing click)', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    const cards = screen.getAllByTestId('post-card');

    fireEvent.mouseDown(cards[0]);
    fireEvent.mouseOver(cards[1]);
    fireEvent.mouseUp(window);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('2 selezionati');

    // The click that the browser fires right after mouseup must not toggle the
    // card under the pointer back off.
    fireEvent.click(cards[1]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('2 selezionati');
  });

  it('starting the sweep on an already-selected card deselects the range', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    const cards = screen.getAllByTestId('post-card');
    fireEvent.click(cards[0]);
    fireEvent.click(cards[1]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('2 selezionati');

    fireEvent.mouseDown(cards[0]);
    fireEvent.mouseOver(cards[1]);
    fireEvent.mouseUp(window);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('0 selezionati');
  });

  it('a plain click (no sweep) still toggles a single card', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    const cards = screen.getAllByTestId('post-card');

    // mousedown + mouseup on the same card without crossing onto another one:
    // the normal click toggle must survive the drag-select plumbing.
    fireEvent.mouseDown(cards[0]);
    fireEvent.mouseUp(window);
    fireEvent.click(cards[0]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');
  });
});

describe('Gallery — active collection', () => {
  it('shows a chip for the active collection source', async () => {
    await renderGallery({
      collectionId: 1,
      collectionLabel: 'Ricette',
      collectionColor: '#e91e63',
    });
    expect(screen.getByTestId('active-collection-chip')).toHaveTextContent('Ricette');
  });
});

describe('Gallery — unified toolbar', () => {
  it('shows the total count exactly once (the old count strip is gone)', async () => {
    await renderGallery();
    expect(screen.queryByTestId('count-strip')).toBeNull();
    expect(screen.getAllByText('2 post')).toHaveLength(1);
    // The "mostrando {n}" suffix went away with the strip.
    expect(screen.queryByText(/mostrando/)).toBeNull();
  });

  it('renders search, sort, zoom, Filtri, refresh and select in one strip', async () => {
    await renderGallery();
    expect(screen.getByPlaceholderText('Search posts...')).toBeInTheDocument();
    expect(screen.getByTestId('sort-toggle')).toBeInTheDocument();
    expect(screen.getByTestId('grid-size-control')).toBeInTheDocument();
    expect(screen.getByTestId('filters-toggle')).toBeInTheDocument();
    expect(screen.getByTestId('gallery-refresh')).toBeInTheDocument();
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
  });

  it('refresh is icon-only with the localized tooltip and still reloads', async () => {
    await renderGallery();
    const refresh = screen.getByTestId('gallery-refresh');
    expect(refresh).toHaveAttribute('title', 'Aggiorna la galleria');
    expect(refresh.textContent).toBe(''); // icon only — no "Aggiorna" label
    const callsBefore = window.electronAPI.getPosts.mock.calls.length;
    fireEvent.click(refresh);
    await waitFor(() =>
      expect(window.electronAPI.getPosts.mock.calls.length).toBeGreaterThan(callsBefore),
    );
  });

  it('every toolbar button carries an explanatory tooltip', async () => {
    await renderGallery({ platform: 'instagram', onSyncSource: vi.fn() });
    expect(screen.getByTestId('sort-toggle')).toHaveAttribute(
      'title',
      'Cambia ordinamento per data',
    );
    expect(screen.getByTestId('filters-toggle')).toHaveAttribute(
      'title',
      'Filtra per tipo di media, download e tag AI',
    );
    expect(screen.getByTestId('gallery-sync-source')).toHaveAttribute(
      'title',
      'Sincronizza questa source dal connettore',
    );
    expect(screen.getByTestId('gallery-refresh')).toHaveAttribute('title', 'Aggiorna la galleria');
    expect(screen.getByTestId('select-toggle')).toHaveAttribute(
      'title',
      'Seleziona più post per azioni in blocco',
    );
  });

  it('the zoom buttons carry the localized grid-size tooltips', async () => {
    await renderGallery();
    const zoom = screen.getByTestId('grid-size-control');
    expect(within(zoom).getByLabelText('Riduci dimensione griglia')).toBeInTheDocument();
    expect(within(zoom).getByLabelText('Aumenta dimensione griglia')).toBeInTheDocument();
  });

  it('select mode replaces the browse toolbar with the action bar', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    // The browse-mode controls give way to the contextual action bar…
    expect(screen.queryByPlaceholderText('Search posts...')).toBeNull();
    expect(screen.queryByTestId('filters-toggle')).toBeNull();
    expect(screen.queryByTestId('gallery-refresh')).toBeNull();
    // …holding the counter, the select-all toggle, the Azioni menu and the exit.
    expect(screen.getByTestId('selection-count')).toBeInTheDocument();
    expect(screen.getByTestId('select-all-matching')).toBeInTheDocument();
    expect(screen.getByTestId('bulk-actions')).toBeInTheDocument();
    expect(screen.getByTestId('select-cancel')).toBeInTheDocument();
  });
});

describe('Gallery — source-sync button', () => {
  const NATIVE_FOLDER = {
    id: 9,
    name: 'Motivational',
    color: '#7B5CFF',
    count: 65,
    platform: 'instagram',
    externalId: '17912345',
  };

  it('shows the button on a syncable platform and starts the platform sync', async () => {
    const onSyncSource = vi.fn();
    await renderGallery({ platform: 'instagram', onSyncSource });
    const btn = screen.getByTestId('gallery-sync-source');
    expect(btn).toHaveAttribute('title', 'Sincronizza questa source dal connettore');
    fireEvent.click(btn);
    expect(onSyncSource).toHaveBeenCalledWith({ type: 'platform', platform: 'instagram' });
  });

  it('hides the button on "all posts", web, and Pinterest without boards', async () => {
    for (const platform of ['all', 'web', 'pinterest']) {
      const { unmount } = await renderGallery({ platform, onSyncSource: vi.fn() });
      expect(screen.queryByTestId('gallery-sync-source')).toBeNull();
      unmount();
    }
  });

  it('targets the collection when viewing a native folder, hides it on custom ones', async () => {
    const onSyncSource = vi.fn();
    const { unmount } = await renderGallery({
      collectionId: 9,
      collections: [...COLLECTIONS, NATIVE_FOLDER],
      onSyncSource,
    });
    fireEvent.click(screen.getByTestId('gallery-sync-source'));
    expect(onSyncSource).toHaveBeenCalledWith({
      type: 'collection',
      platform: 'instagram',
      collectionId: 9,
    });
    unmount();
    // Custom folder (no platform/externalId) → not syncable.
    await renderGallery({ collectionId: 1, onSyncSource: vi.fn() });
    expect(screen.queryByTestId('gallery-sync-source')).toBeNull();
  });

  it('turns into a stop spinner while that platform run is in flight', async () => {
    await renderGallery({
      platform: 'instagram',
      onSyncSource: vi.fn(),
      sourceSyncJobs: { instagram: { status: 'syncing' } },
    });
    const btn = screen.getByTestId('gallery-sync-source');
    expect(btn).toHaveAttribute('title', 'Interrompi la sincronizzazione');
    expect(btn.querySelector('.animate-spin')).toBeTruthy();
  });
});

describe('Gallery — quick-select (hover checkbox contract)', () => {
  it('onQuickSelect arms select mode with exactly that post selected', async () => {
    await renderGallery();
    expect(screen.queryByTestId('selection-count')).toBeNull();
    expect(typeof gridProps.current.onQuickSelect).toBe('function');

    act(() => gridProps.current.onQuickSelect(POSTS[1]));

    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');
    const cards = screen.getAllByTestId('post-card');
    expect(cards[1]).toHaveAttribute('data-selected', 'true');
    expect(cards[0]).toHaveAttribute('data-selected', 'false');
  });

  it('onQuickSelect is a no-op while already in select mode (the card click toggles)', async () => {
    await renderGallery();
    fireEvent.click(screen.getByTestId('select-toggle'));
    fireEvent.click(screen.getAllByTestId('post-card')[0]);
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');

    act(() => gridProps.current.onQuickSelect(POSTS[1]));
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1 selezionati');
  });
});
