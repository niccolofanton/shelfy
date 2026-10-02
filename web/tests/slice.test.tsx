// The desktop UI on the web client: App, Gallery, Sidebar and the post modal
// against a mock ShelfyClient with the web capabilities, and no preload bridge
// at all, as in a browser.
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest';
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
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
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
      'add-source-btn',
      'edit-collection-1',
      'nav-downloads',
      'nav-ai',
      'nav-feedback',
      'nav-settings',
      'select-toggle',
      'gallery-sync-source',
    ]) {
      expect(screen.queryByTestId(id), id).toBeNull();
    }
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
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'lamp' } });
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

  it('has no selection: no select button, no quick-select on hover', async () => {
    renderGallery();
    const [card] = await screen.findAllByTestId('post-card');
    fireEvent.mouseEnter(card);
    expect(screen.queryByTestId('select-toggle')).toBeNull();
    expect(screen.queryByTestId('quick-select-checkbox')).toBeNull();
  });

  it('shows stored covers through their grid rendition', async () => {
    renderGallery();
    const [card] = await screen.findAllByTestId('post-card');
    expect(within(card).getByTestId('card-image')).toHaveAttribute('src', '/media/aa.g480.webp');
  });
});

describe('Post modal on the web client', () => {
  function renderModal(post: Shelfy.Post, client = webClient()) {
    render(
      <ShelfyProvider client={client}>
        <PostModal post={post} onClose={vi.fn()} />
      </ShelfyProvider>,
    );
    return client;
  }

  it('is read-only: the AI layer, the note and the tags without editing', () => {
    renderModal(POSTS[0]);
    const modal = screen.getByTestId('post-modal');
    expect(within(modal).getByText('A blown-glass lamp')).toBeInTheDocument();
    expect(within(modal).getByText('#glass')).toBeInTheDocument();
    expect(within(modal).getByText('#mine')).toBeInTheDocument();
    expect(within(modal).getByTestId('post-modal-note')).toHaveTextContent('For the hall');
    for (const id of [
      'post-modal-edit',
      'post-modal-ai-more',
      'post-modal-regenerate',
      'post-modal-analyze',
      'post-modal-manual-tag-input',
      'post-modal-note-add',
      'post-modal-assign-toggle',
    ]) {
      expect(within(modal).queryByTestId(id), id).toBeNull();
    }
    expect(screen.getByTestId('post-modal-image')).toHaveAttribute('src', '/media/aa.jpg');
  });

  it('offers only "open original", through the client', () => {
    const client = renderModal(POSTS[0]);
    fireEvent.click(screen.getByTestId('post-modal-more'));
    const menu = screen.getByTestId('post-modal-menu');
    expect(within(menu).getAllByRole('button')).toHaveLength(1);
    fireEvent.click(within(menu).getByTestId('post-modal-external'));
    expect(client.openExternal).toHaveBeenCalledWith('https://www.instagram.com/p/C0ffee/');
  });

  it('opens the original page where the desktop would embed it', () => {
    const tweet = POSTS[1];
    const client = renderModal({ ...tweet, thumbnailUrl: null });
    expect(screen.getByTestId('post-modal-no-media')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('post-modal-open-original'));
    expect(client.openExternal).toHaveBeenCalledWith('https://x.com/i/web/status/2');
  });

  it('has no menu for a post with nothing to open', () => {
    renderModal({ ...POSTS[0], postUrl: null });
    expect(screen.queryByTestId('post-modal-more')).toBeNull();
  });
});
