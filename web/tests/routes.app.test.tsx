// App on the web client under the address bar (WebNavigation): deep links
// render, moving around changes the address, the back button follows, and the
// live events reach the shell. A mock ShelfyClient and no preload bridge, as
// in a browser.
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest';
import React from 'react';
import { render, screen, fireEvent, waitFor, within, act } from '@testing-library/react';
import App from '@ui/App';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient, ShelfyEvent } from '@ui/api/ShelfyClient';
import type { ElectronAPI } from '../../types/electron-api';
import { ApiError } from '../src/api/http';
import { WEB_CAPABILITIES } from '../src/api/httpClient';
import { toPost, webMedia } from '../src/api/mapping';
import { WebNavigation } from '../src/routes';
import { apiPost } from './fixtures';

const POSTS = [
  toPost(apiPost({ key: 'ig_1', caption: 'Blown-glass lamp', collectionIds: [1] })),
  toPost(apiPost({ key: 'x_2', platform: 'twitter', caption: 'Chairs', mediaType: 'text' })),
];

const FOLDERS: Shelfy.Collection[] = [
  {
    id: 1,
    name: 'Lighting',
    color: '#3d5afe',
    platform: null,
    externalId: null,
    igName: null,
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

const STATS: Shelfy.Stats = {
  total: 2,
  byPlatform: { instagram: 1, twitter: 1, pinterest: 0, web: 0 },
  byMediaType: { image: 1, text: 1 },
  downloaded: 0,
  downloadedByType: { thumbnails: 0, images: 0, videos: 0 },
};

type MockClient = ShelfyClient & {
  listPosts: Mock;
  getPostsByIds: Mock;
  getStats: Mock;
  listCollections: Mock;
  reportError: Mock;
  emit: (event: ShelfyEvent) => void;
};

function webClient(): MockClient {
  const listeners = new Map<string, ((event: ShelfyEvent) => void)[]>();
  return {
    capabilities: WEB_CAPABILITIES,
    media: webMedia,
    listPosts: vi.fn(async (query: { collectionId?: number | null }) => {
      const posts = POSTS.filter(
        (p) => !query.collectionId || p.collectionIds?.includes(query.collectionId),
      );
      return { posts, total: posts.length, nextCursor: null };
    }),
    getPostsByIds: vi.fn(async (ids: string[]) => POSTS.filter((p) => ids.includes(p.id))),
    getStats: vi.fn().mockResolvedValue(STATS),
    listCollections: vi.fn().mockResolvedValue(FOLDERS),
    openExternal: vi.fn(),
    on: vi.fn((type: string, listener: (event: ShelfyEvent) => void) => {
      listeners.set(type, [...(listeners.get(type) ?? []), listener]);
      return () => {};
    }) as ShelfyClient['on'],
    reportError: vi.fn(),
    emit: (event) => (listeners.get(event.type) ?? []).forEach((listener) => listener(event)),
  };
}

function renderAt(path: string, client = webClient()): MockClient {
  window.history.replaceState(null, '', path);
  render(
    <ShelfyProvider client={client}>
      <WebNavigation>
        <App />
      </WebNavigation>
    </ShelfyProvider>,
  );
  return client;
}

const lastQuery = (client: MockClient) => client.listPosts.mock.calls.at(-1)?.[0];

// A browser has no preload bridge: any call to it would throw.
let bridge: ElectronAPI;
beforeEach(() => {
  bridge = window.electronAPI;
  delete (window as Partial<Window>).electronAPI;
});
afterEach(() => {
  window.electronAPI = bridge;
  window.history.replaceState(null, '', '/');
});

describe('deep links', () => {
  it('/ shows the whole library', async () => {
    const client = renderAt('/');
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    expect(lastQuery(client)).toMatchObject({ collectionId: undefined });
  });

  it('/c/:collectionId opens the folder at once', async () => {
    const client = renderAt('/c/1');
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(1));
    expect(client.listPosts).toHaveBeenCalledTimes(1);
    expect(lastQuery(client)).toMatchObject({ collectionId: 1 });
    await screen.findByTestId('active-collection-chip');
  });

  it('/p/:key opens the post over the library; closing a deep link goes to /', async () => {
    const client = renderAt('/p/x_2');
    const modal = await screen.findByTestId('post-modal');
    expect(within(modal).getByText('Chairs')).toBeInTheDocument();
    expect(client.getPostsByIds).toHaveBeenCalledWith(['x_2']);
    fireEvent.click(within(modal).getByTestId('post-modal-close'));
    await waitFor(() => expect(screen.queryByTestId('post-modal')).toBeNull());
    expect(window.location.pathname).toBe('/');
  });

  it('says so for the pages this client cannot show yet', async () => {
    renderAt('/trash');
    expect(await screen.findByTestId('route-unavailable')).toBeInTheDocument();
    act(() => window.history.pushState(null, '', '/settings/legal'));
    await waitFor(() =>
      expect(screen.getAllByTestId('route-unavailable').length).toBeGreaterThan(0),
    );
    fireEvent.click(screen.getAllByTestId('route-back')[0]);
    expect(window.location.pathname).toBe('/');
  });

  it('has a not-found page for an address with nothing behind it', async () => {
    renderAt('/nowhere');
    const panel = await screen.findByTestId('route-not-found');
    expect(panel).toHaveTextContent('Pagina non trovata');
    fireEvent.click(within(panel).getByTestId('route-back'));
    await waitFor(() => expect(screen.queryByTestId('route-not-found')).toBeNull());
    expect(window.location.pathname).toBe('/');
  });

  it('says why a post cannot be opened', async () => {
    renderAt('/p/ig_404');
    expect(await screen.findByTestId('route-not-found')).toHaveTextContent(
      'Questo post non è nella tua libreria',
    );
    cleanupAndRender('/p/ig_1', (client) =>
      client.getPostsByIds.mockRejectedValue(new ApiError(0, 'network')),
    );
    expect(await screen.findByTestId('route-not-found')).toHaveTextContent(
      'Il server non risponde',
    );
  });
});

// Re-renders the app at `path` with a client the caller adjusts.
function cleanupAndRender(path: string, adjust: (client: MockClient) => void): void {
  document.body.innerHTML = '';
  const client = webClient();
  adjust(client);
  renderAt(path, client);
}

describe('moving around', () => {
  it('gives a folder its address, and the back button returns', async () => {
    const client = renderAt('/');
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    fireEvent.click(within(screen.getByTestId('sidebar')).getByText('Inspiration'));
    expect(window.location.pathname).toBe('/c/2');
    await waitFor(() => expect(lastQuery(client)).toMatchObject({ collectionId: 2 }));

    act(() => window.history.back());
    await waitFor(() => expect(window.location.pathname).toBe('/'));
    await waitFor(() => expect(lastQuery(client)).toMatchObject({ collectionId: undefined }));
  });

  it('keeps the address for a platform, which lives on the library', async () => {
    const client = renderAt('/c/1');
    await waitFor(() => expect(lastQuery(client)).toMatchObject({ collectionId: 1 }));
    fireEvent.click(within(screen.getByTestId('sidebar')).getByTestId('source-all'));
    expect(window.location.pathname).toBe('/');
    await waitFor(() => expect(lastQuery(client)).toMatchObject({ collectionId: undefined }));
  });
});

describe('live events in the shell', () => {
  it('refreshes the stats on stats.changed, and everything on resync', async () => {
    const client = renderAt('/');
    await waitFor(() => expect(screen.getAllByTestId('post-card')).toHaveLength(2));
    await waitFor(() => expect(client.getStats).toHaveBeenCalledTimes(1));
    const folders = client.listCollections.mock.calls.length;

    act(() => client.emit({ type: 'stats.changed' }));
    await waitFor(() => expect(client.getStats).toHaveBeenCalledTimes(2));

    // A posts.changed alone does not touch the counters on the web.
    act(() => client.emit({ type: 'posts.changed', keys: ['ig_1'], reason: 'edit' }));
    const lists = client.listPosts.mock.calls.length;
    await waitFor(() => expect(client.listPosts.mock.calls.length).toBe(lists + 1));
    expect(client.getStats).toHaveBeenCalledTimes(2);

    act(() => client.emit({ type: 'resync' }));
    await waitFor(() => expect(client.listCollections.mock.calls.length).toBe(folders + 1));
    await waitFor(() => expect(client.listPosts.mock.calls.length).toBe(lists + 2));
  });
});

describe('error boundaries', () => {
  it('a crashing view shows its panel, keeps the shell and reports through the client', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    // A stored cover whose reference the client cannot resolve: the card
    // throws while rendering.
    const client: MockClient = {
      ...webClient(),
      media: {
        ...webMedia,
        tile: () => {
          throw new TypeError('bad media reference');
        },
      },
    };
    client.listPosts.mockResolvedValue({
      posts: [{ ...POSTS[0], thumbnailPath: '/media/aa.jpg' }],
      total: 1,
      nextCursor: null,
    });
    renderAt('/', client);
    expect(await screen.findByTestId('error-boundary')).toBeInTheDocument();
    expect(screen.getByTestId('sidebar')).toBeInTheDocument();
    expect(client.reportError).toHaveBeenCalledWith(
      expect.objectContaining({ view: 'gallery', error: expect.any(Error) }),
    );
    consoleError.mockRestore();
  });
});
