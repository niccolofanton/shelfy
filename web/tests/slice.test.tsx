// The desktop UI on the web client: App, Gallery, Sidebar and the post modal
// against a mock ShelfyClient with the web capabilities, and no preload bridge
// at all, as in a browser.
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest';
import type { ComponentProps } from 'react';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import App from '@ui/App';
import Gallery from '@ui/views/Gallery';
import PostModal from '@ui/components/PostModal';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import type { ElectronAPI } from '../../types/electron-api';
import { WEB_CAPABILITIES } from '../src/api/httpClient';
import { toPost, webMedia } from '../src/api/mapping';
import { apiObject, apiPost, apiSlide } from './fixtures';

type MockClient = ShelfyClient & {
  listPosts: Mock;
  getStats: Mock;
  listCollections: Mock;
  openExternal: Mock;
  updatePost: Mock;
  addPostsToCollections: Mock;
  removePostFromCollection: Mock;
};

const POSTS = [
  toPost(
    apiPost({
      key: 'ig_1',
      cover: apiObject('aa'),
      aiDescription: 'A blown-glass lamp',
      aiTags: ['glass', 'lamp'],
      aiStatus: 'done',
      userNote: 'For the hall',
      userTags: ['mine'],
      media: [apiSlide({ object: apiObject('aa') })],
    }),
  ),
  toPost(apiPost({ key: 'x_2', platform: 'twitter', postUrl: null, mediaType: 'text' })),
];

const STATS: Shelfy.Stats = {
  total: 2,
  byPlatform: { instagram: 1, twitter: 1, pinterest: 0, web: 0 },
  byMediaType: { image: 1, text: 1 },
  downloaded: 1,
  downloadedByType: { thumbnails: 1, images: 0, videos: 0 },
};

const FOLDERS: Shelfy.Collection[] = [
  {
    id: 1,
    name: 'Lighting',
    color: '#3d5afe',
    platform: 'instagram',
    externalId: '179',
    igName: 'lighting',
    count: 1,
  },
  {
    id: 2,
    name: 'Inspiration',
    color: '#ffaa00',
    platform: null,
    externalId: null,
    igName: null,
    count: 0,
  },
];

function webClient(): MockClient {
  return {
    capabilities: WEB_CAPABILITIES,
    media: webMedia,
    listPosts: vi.fn().mockResolvedValue({ posts: POSTS, total: POSTS.length, nextCursor: null }),
    getPostsByIds: vi.fn().mockResolvedValue([]),
    getStats: vi.fn().mockResolvedValue(STATS),
    listCollections: vi.fn().mockResolvedValue(FOLDERS),
    countPosts: vi.fn().mockResolvedValue(0),
    resolveAllIds: vi.fn().mockResolvedValue(null),
    bulkAction: vi.fn(),
    listTrash: vi.fn(),
    restoreFromTrash: vi.fn(),
    emptyTrash: vi.fn(),
    updatePost: vi.fn(),
    createCollection: vi.fn(),
    updateCollection: vi.fn(),
    deleteCollection: vi.fn(),
    addPostsToCollections: vi.fn(),
    removePostFromCollection: vi.fn(),
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
    reportError: vi.fn(),
  };
}

// A browser has no preload bridge: any call to it would throw.
let bridge: ElectronAPI;
beforeEach(() => {
  bridge = window.electronAPI;
  delete (window as Partial<Window>).electronAPI;
});
afterEach(() => {
  window.electronAPI = bridge;
});

describe('App on the web client', () => {
  it('shows the library and nothing the web cannot do yet', async () => {
    const client = webClient();
    render(
      <ShelfyProvider client={client}>
        <App />
      </ShelfyProvider>,
    );
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    const sidebar = screen.getByTestId('sidebar');
    expect(within(sidebar).getByTestId('source-all')).toHaveTextContent('2');
    expect(within(sidebar).getByTestId('source-instagram')).toBeInTheDocument();
    expect(within(sidebar).getByText('Lighting')).toBeInTheDocument();
    expect(within(sidebar).getByText('Inspiration')).toBeInTheDocument();
    for (const id of [
      'nav-browser',
      'browser-tab-instagram',
      'browser-tab-add-site',
      'browser-tab-add-bookmark',
      'nav-downloads',
      'nav-ai',
      'nav-feedback',
      'nav-settings',
      'gallery-sync-source',
    ]) {
      expect(screen.queryByTestId(id), id).toBeNull();
    }
    // libraryEdit (P1-06): folders can be created and edited.
    expect(within(sidebar).getByTestId('add-source-btn')).toBeInTheDocument();
    expect(within(sidebar).getByTestId('edit-collection-1')).toBeInTheDocument();
    // bulkActions (P1-14): selection and the Trash nav entry are now on.
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
    expect(within(sidebar).getByTestId('nav-trash')).toBeInTheDocument();
    expect(client.getStats).toHaveBeenCalled();
    expect(client.listCollections).toHaveBeenCalled();
  });

  it('filters the gallery from a sidebar folder', async () => {
    const client = webClient();
    render(
      <ShelfyProvider client={client}>
        <App />
      </ShelfyProvider>,
    );
    fireEvent.click(await within(screen.getByTestId('sidebar')).findByText('Lighting'));
    await waitFor(() =>
      expect(client.listPosts).toHaveBeenLastCalledWith(
        expect.objectContaining({ collectionId: 1 }),
        expect.objectContaining({ cursor: null }),
      ),
    );
  });
});

describe('Gallery on the web client', () => {
  function renderGallery(client = webClient()) {
    render(
      <ShelfyProvider client={client}>
        <Gallery collections={FOLDERS} />
      </ShelfyProvider>,
    );
    return client;
  }

  it('loads the first page, then searches', async () => {
    const client = renderGallery();
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    expect(client.listPosts).toHaveBeenCalledWith(
      expect.objectContaining({ platform: undefined, sortOrder: 'newest' }),
      expect.objectContaining({ limit: 50, cursor: null }),
    );
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'lamp' } });
    await waitFor(
      () =>
        expect(client.listPosts).toHaveBeenLastCalledWith(
          expect.objectContaining({ search: 'lamp' }),
          expect.objectContaining({ cursor: null }),
        ),
      { timeout: 2000 },
    );
  });

  it('filters by media type from the drawer', async () => {
    const client = renderGallery();
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    fireEvent.click(screen.getByTestId('filters-toggle'));
    fireEvent.click(within(screen.getByTestId('drawer-mediatype')).getByText('Video'));
    await waitFor(() =>
      expect(client.listPosts).toHaveBeenLastCalledWith(
        expect.objectContaining({ mediaType: 'video' }),
        expect.anything(),
      ),
    );
  });

  it('has selection (P1-14 bulkActions): a select button, and quick-select on hover', async () => {
    renderGallery();
    const [card] = await screen.findAllByTestId('post-card');
    expect(screen.getByTestId('select-toggle')).toBeInTheDocument();
    fireEvent.mouseEnter(card);
    fireEvent.click(await screen.findByTestId('quick-select-checkbox'));
    expect(screen.getByTestId('selection-count')).toHaveTextContent('1');
  });

  it('shows stored covers through their grid rendition', async () => {
    renderGallery();
    const [card] = await screen.findAllByTestId('post-card');
    expect(within(card).getByTestId('card-image')).toHaveAttribute('src', '/media/aa.g480.webp');
  });
});

describe('Post modal on the web client', () => {
  function renderModal(
    post: Shelfy.Post,
    client = webClient(),
    props: Partial<ComponentProps<typeof PostModal>> = {},
  ) {
    render(
      <ShelfyProvider client={client}>
        <PostModal post={post} onClose={vi.fn()} {...props} />
      </ShelfyProvider>,
    );
    return client;
  }

  it('is editable (P1-06 libraryEdit, P1-14 bulkActions): the note, tags, AI layer and AI-clear menu; not yet regenerate', () => {
    renderModal(POSTS[0]);
    const modal = screen.getByTestId('post-modal');
    expect(within(modal).getByText('A blown-glass lamp')).toBeInTheDocument();
    expect(within(modal).getByText('#glass')).toBeInTheDocument();
    expect(within(modal).getByText('#mine')).toBeInTheDocument();
    expect(within(modal).getByTestId('post-modal-note')).toHaveTextContent('For the hall');
    for (const id of [
      'post-modal-edit',
      'post-modal-manual-tag-input',
      'post-modal-assign-toggle',
      // The bulk AI-clear menu (`bulkActions`, the same seam as the gallery's
      // bulk bar) is on since P1-14.
      'post-modal-ai-more',
    ]) {
      expect(within(modal).queryByTestId(id), id).not.toBeNull();
    }
    // Regenerating an analysis still waits on its own capability (`ai`).
    for (const id of ['post-modal-regenerate', 'post-modal-analyze']) {
      expect(within(modal).queryByTestId(id), id).toBeNull();
    }
    expect(screen.getByTestId('post-modal-image')).toHaveAttribute('src', '/media/aa.jpg');
  });

  it('saves an edited note through the seam (PATCH /posts/{key})', async () => {
    const client = renderModal(POSTS[0]);
    fireEvent.click(screen.getByTestId('post-modal-note'));
    fireEvent.change(screen.getByTestId('post-modal-note-input'), {
      target: { value: 'Updated note' },
    });
    fireEvent.click(screen.getByTestId('post-modal-note-save'));
    await waitFor(() =>
      expect(client.updatePost).toHaveBeenCalledWith('ig_1', { userNote: 'Updated note' }),
    );
  });

  it('adds a manual tag through the seam', async () => {
    const client = renderModal(POSTS[0]);
    const input = screen.getByTestId('post-modal-manual-tag-input');
    fireEvent.change(input, { target: { value: 'new-tag' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() =>
      expect(client.updatePost).toHaveBeenCalledWith('ig_1', { userTags: ['mine', 'new-tag'] }),
    );
  });

  it('adds this post to a folder, and the same picker removes it again (§1.2 #12)', async () => {
    const client = renderModal(POSTS[0]);
    fireEvent.click(screen.getByTestId('post-modal-assign-toggle'));
    const toLighting = await screen.findByTestId('post-modal-assign-to-1');
    fireEvent.click(toLighting);
    await waitFor(() => expect(client.addPostsToCollections).toHaveBeenCalledWith(['ig_1'], [1]));
    fireEvent.click(toLighting); // already a member now: the same row removes it
    await waitFor(() => expect(client.removePostFromCollection).toHaveBeenCalledWith('ig_1', 1));
  });

  it('offers "open original", "download original" and, since P1-14, "delete"', () => {
    const client = renderModal(POSTS[0]);
    fireEvent.click(screen.getByTestId('post-modal-more'));
    // The menu is a portal now (anchored on desktop, a sheet on narrow), so it
    // is reached through `screen`, not within the modal; its rows are menuitems.
    const menu = screen.getByTestId('post-modal-menu');
    expect(within(menu).getAllByRole('menuitem')).toHaveLength(3);
    expect(within(menu).getByTestId('post-modal-download-original')).toHaveAttribute(
      'href',
      '/media/aa.jpg',
    );
    expect(within(menu).getByTestId('post-modal-delete-post')).toBeInTheDocument();
    fireEvent.click(within(menu).getByTestId('post-modal-external'));
    expect(client.openExternal).toHaveBeenCalledWith('https://www.instagram.com/p/C0ffee/');
  });

  it('deletes the post through the bulk seam (P1-14), with its undo handle', async () => {
    const client = webClient();
    vi.mocked(client.bulkAction).mockResolvedValue({
      changed: 1,
      selected: 1,
      deletedAt: 5_000,
      job: null,
    });
    const onPostDeleted = vi.fn();
    renderModal(POSTS[0], client, { onPostDeleted });
    const modal = screen.getByTestId('post-modal');
    fireEvent.click(within(modal).getByTestId('post-modal-more'));
    // The menu is portaled out of the modal: reach the item through `screen`.
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post')); // two-step confirm
    await waitFor(() =>
      expect(client.bulkAction).toHaveBeenCalledWith({ keys: ['ig_1'] }, 'delete'),
    );
    await waitFor(() => expect(onPostDeleted).toHaveBeenCalledWith('ig_1', 5_000));
  });

  it('shows the media fallback with "open original" for a post whose media is not stored', () => {
    // A non-text post with nothing downloaded: the media pane shows the fallback
    // (platform glyph, "Media not available", Open original), not the browser's
    // broken-image icon (MOD-2). A text post instead gets its TextCard (MOD-3).
    const client = renderModal({ ...POSTS[1], mediaType: 'image', thumbnailUrl: null });
    expect(screen.getByTestId('post-modal-no-media')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('post-modal-open-original'));
    expect(client.openExternal).toHaveBeenCalledWith('https://x.com/i/web/status/2');
  });

  it('renders a text-only post as a TextCard in the media pane, shown once (MOD-3)', () => {
    // A text tweet with nothing stored: its caption is the hero in the media
    // pane (TextCard), not a globe + "open original", and not repeated in meta.
    renderModal({
      ...POSTS[1],
      mediaType: 'text',
      text: 'A thread about chairs',
      media: [],
      thumbnailUrl: null,
      imagePath: null,
      thumbnailPath: null,
      videoPath: null,
      previewPath: null,
    });
    const media = screen.getByTestId('post-modal-media');
    expect(within(media).getByTestId('post-modal-text')).toHaveTextContent('A thread about chairs');
    expect(screen.queryByTestId('post-modal-no-media')).toBeNull();
    expect(screen.queryByTestId('post-modal-image')).toBeNull();
    // The caption shows exactly once (the TextCard), not duplicated in meta.
    expect(screen.getAllByText('A thread about chairs')).toHaveLength(1);
  });

  it('still offers Delete (P1-14 bulkActions) for a post with nothing to open and nothing stored', () => {
    renderModal({
      ...POSTS[0],
      postUrl: null,
      thumbnailPath: null,
      videoPath: null,
      imagePath: null,
      media: [],
    });
    fireEvent.click(screen.getByTestId('post-modal-more'));
    const menu = screen.getByTestId('post-modal-menu');
    expect(within(menu).getByTestId('post-modal-delete-post')).toBeInTheDocument();
    expect(within(menu).queryByTestId('post-modal-external')).toBeNull();
    expect(within(menu).queryByTestId('post-modal-download-original')).toBeNull();
  });
});
