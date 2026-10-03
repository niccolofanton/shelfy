import React from 'react';
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import Gallery from '../../src/views/Gallery';
import FilterDrawer from '../../src/components/FilterDrawer';
import { I18nProvider } from '../../src/i18n';
import { NavigationProvider } from '../../src/api/navigation';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import type { ShelfyClient } from '../../src/api/ShelfyClient';
import type { AiProvidersApi, AiProviderSettings } from '../../src/api/aiProviders';
import type { LibraryFacets, LibraryFacetsApi } from '../../src/api/facets';
import { notifyProviderSettingsChanged } from '../../src/components/ai/providerConnection';
import { createHttpClient, WEB_CAPABILITIES } from '../src/api/httpClient';
import { createSuggestApi } from '../src/api/ai/suggest';
import { createFacetsApi } from '../src/api/facets';
import { countParams, listPostsParams, toPost, toPostSelector, webMedia } from '../src/api/mapping';
import { fakeHttp, OWNER } from './authFakes';
import { apiPost } from './fixtures';
import { usePosts } from '../../src/hooks/usePosts';

const facets: LibraryFacets = {
  category: [
    { value: 'technology', count: 3 },
    { value: 'new-category', count: 1 },
  ],
  contentType: [{ value: 'portfolio', count: 2 }],
  status: [
    { value: 'done', count: 3 },
    { value: 'none', count: 1 },
  ],
  language: [
    { value: 'it', count: 3 },
    { value: 'en', count: 1 },
  ],
};
function settings(enabled: boolean): AiProviderSettings {
  return {
    aiRouting: {},
    aiConcurrency: 1,
    aiSuggestions: enabled,
    aiVisionQc: false,
    aiAutoAnalyzeWebsites: false,
    aiDictationInterim: false,
  };
}
function gallery(enabled = true) {
  const preferences = { value: settings(enabled) };
  const providers = {
    getSettings: vi.fn(async () => preferences.value),
    onResync: () => () => {},
  } as unknown as AiProvidersApi;
  const suggest = vi.fn(async () => ({ tags: ['glass', 'lighting'] }));
  const post = toPost(apiPost({ mediaType: 'text', caption: 'Lamp', coverUrl: null }));
  const listPosts = vi.fn(async () => ({ posts: [post], total: 1, nextCursor: null }));
  const client = {
    capabilities: { ...WEB_CAPABILITIES, ai: true, aiSuggest: true },
    media: webMedia,
    ai: { suggest: { suggest } },
    aiProviders: providers,
    listPosts,
    countPosts: vi.fn(async () => 1),
    on: () => () => {},
    reportError: vi.fn(),
  } as unknown as ShelfyClient;
  localStorage.setItem('app:language', 'en');
  const node = (active: boolean) => (
    <I18nProvider>
      <ShelfyProvider client={client}>
        <NavigationProvider
          navigation={{ route: { name: 'library' }, navigate: vi.fn(), back: vi.fn() }}
        >
          <Gallery active={active} />
        </NavigationProvider>
      </ShelfyProvider>
    </I18nProvider>
  );
  const view = render(node(true));
  return {
    ...view,
    suggest,
    listPosts,
    preferences,
    setActive: (active: boolean) => view.rerender(node(active)),
  };
}
async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  localStorage.removeItem('app:language');
});

describe('web Gallery AI', () => {
  it('does not start provider work while the kept-alive Gallery is hidden', async () => {
    const h = gallery();
    await screen.findByTestId('post-card');
    vi.useFakeTimers();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search posts' }), {
      target: { value: 'lamp' },
    });
    await advance(300);
    h.setActive(false);
    await advance(1000);
    expect(h.suggest).not.toHaveBeenCalled();
    h.setActive(true);
    await advance(600);
    expect(h.suggest).toHaveBeenCalledOnce();
  });
  it('waits 600ms after the committed query and adds OR concepts with an AND toggle', async () => {
    const h = gallery();
    await screen.findByTestId('post-card');
    vi.useFakeTimers();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search posts' }), {
      target: { value: 'lamp' },
    });
    await advance(300); // The existing search-input debounce commits the query.
    await advance(599);
    expect(h.suggest).not.toHaveBeenCalled();
    await advance(1);
    expect(h.suggest).toHaveBeenCalledWith('lamp', { scope: 'all' });
    expect(window.electronAPI.suggestSearch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'glass' }));
    await advance(1);
    expect(h.listPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ search: 'lamp', concepts: ['glass'], conceptMode: 'or' }),
      expect.any(Object),
    );
    fireEvent.click(screen.getByRole('button', { name: 'lighting' }));
    fireEvent.click(screen.getByTestId('concept-mode-and'));
    await advance(1);
    expect(h.listPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ concepts: ['glass', 'lighting'], conceptMode: 'and' }),
      expect.any(Object),
    );
    await advance(2000);
    expect(screen.getByTestId('post-card')).toBeVisible();
  });
  it('uses the account opt-in and reacts to a saved preference change', async () => {
    localStorage.setItem('aiSearchSuggestions', 'true');
    const h = gallery(false);
    await screen.findByTestId('post-card');
    vi.useFakeTimers();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search posts' }), {
      target: { value: 'lamp' },
    });
    await advance(1000);
    expect(h.suggest).not.toHaveBeenCalled();
    h.preferences.value = settings(true);
    await act(async () => notifyProviderSettingsChanged({}));
    await advance(600);
    expect(h.suggest).toHaveBeenCalledOnce();
    h.preferences.value = settings(false);
    await act(async () => notifyProviderSettingsChanged({}));
    expect(screen.queryByTestId('suggested-tags-bar')).toBeNull();
    localStorage.removeItem('aiSearchSuggestions');
  });
  it('ignores an in-flight answer after the query is cleared or the view is closed', async () => {
    const h = gallery();
    await screen.findByTestId('post-card');
    let resolve!: (result: { tags: string[] }) => void;
    h.suggest.mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    vi.useFakeTimers();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search posts' }), {
      target: { value: 'lamp' },
    });
    await advance(300);
    await advance(600);
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search posts' }), {
      target: { value: '' },
    });
    await advance(300);
    await act(async () => resolve({ tags: ['stale'] }));
    expect(screen.queryByTestId('suggested-tags-bar')).toBeNull();
    h.unmount();
  });
  it('renders live facet counts, unknown vocabulary, language and reset in Italian', async () => {
    localStorage.setItem('app:language', 'it');
    const changed = vi.fn();
    const api: LibraryFacetsApi = { get: vi.fn(async () => facets), onChanged: () => () => {} };
    render(
      <I18nProvider>
        <FilterDrawer
          open
          onClose={vi.fn()}
          filters={{ aiLanguage: 'it' }}
          onChange={changed}
          showAiStatus
          facetsApi={api}
        />
      </I18nProvider>,
    );
    expect(await screen.findByRole('option', { name: 'Tecnologia (3)' })).toBeVisible();
    expect(screen.getByRole('option', { name: 'new-category (1)' })).toBeVisible();
    expect(screen.getByRole('option', { name: 'Non analizzato (1)' })).toBeVisible();
    const language = screen.getByTestId('drawer-language-select');
    expect(within(language).getByRole('option', { name: 'italiano (3)' })).toBeVisible();
    fireEvent.change(language, { target: { value: 'en' } });
    expect(changed).toHaveBeenLastCalledWith({ aiLanguage: 'en' });
    fireEvent.click(screen.getByTestId('drawer-reset'));
    expect(changed).toHaveBeenLastCalledWith(
      expect.objectContaining({
        aiLanguage: undefined,
        aiStatus: undefined,
        category: undefined,
        contentType: undefined,
      }),
    );
  });
  it('recovers a failed facet read without showing fabricated static counts', async () => {
    localStorage.setItem('app:language', 'en');
    const get = vi.fn().mockRejectedValueOnce({ code: 'network' }).mockResolvedValue(facets);
    render(
      <I18nProvider>
        <FilterDrawer
          open
          onClose={vi.fn()}
          filters={{}}
          onChange={vi.fn()}
          showAiStatus
          facetsApi={{ get, onChanged: () => () => {} }}
        />
      </I18nProvider>,
    );
    await screen.findByRole('alert');
    expect(screen.queryByRole('option', { name: 'Technology' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByRole('option', { name: 'Technology (3)' })).toBeVisible();
  });
  it('preserves language and missing-status filters across listing, counts and select-all bulk', () => {
    const query = { aiLanguage: 'it', aiStatus: 'none' };
    expect(listPostsParams(query, { limit: 60 })).toMatchObject(query);
    expect(countParams(query)).toMatchObject(query);
    expect(toPostSelector({ filter: query, exceptKeys: ['ig_1'] })).toEqual({
      filter: query,
      exceptKeys: ['ig_1'],
    });
  });
  it('starts a fresh list when only language changes and refreshes AI-dependent membership', async () => {
    const listeners = new Map<string, (value: unknown) => void>();
    const listPosts = vi.fn(async () => ({ posts: [], total: 0, nextCursor: null }));
    const getPostsByIds = vi.fn(async () => []);
    const client = {
      listPosts,
      getPostsByIds,
      on: (name: string, fn: (value: unknown) => void) => {
        listeners.set(name, fn);
        return () => listeners.delete(name);
      },
    } as unknown as ShelfyClient;
    const view = renderHook(({ aiLanguage }) => usePosts({ aiLanguage, limit: 60 }), {
      initialProps: { aiLanguage: 'it' },
      wrapper: ({ children }) => <ShelfyProvider client={client}>{children}</ShelfyProvider>,
    });
    await waitFor(() => expect(listPosts).toHaveBeenCalledOnce());
    view.rerender({ aiLanguage: 'en' });
    await waitFor(() => expect(listPosts).toHaveBeenCalledTimes(2));
    expect(listPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ aiLanguage: 'en' }),
      expect.objectContaining({ cursor: null }),
    );
    vi.useFakeTimers();
    act(() => listeners.get('post.analyzed')?.({ postId: 'ig_1' }));
    await advance(500);
    expect(listPosts).toHaveBeenCalledTimes(3);
    expect(getPostsByIds).not.toHaveBeenCalled();
  });
  it('aborts closing facet reads and ignores their stale answer after reopening', async () => {
    let resolve!: (value: LibraryFacets) => void;
    const get = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise((done) => {
            resolve = done;
          }),
      )
      .mockResolvedValue(facets);
    const off = vi.fn();
    const api = { get, onChanged: () => off };
    const ui = (open: boolean) => (
      <I18nProvider>
        <FilterDrawer
          open={open}
          onClose={vi.fn()}
          filters={{}}
          onChange={vi.fn()}
          showAiStatus
          facetsApi={api}
        />
      </I18nProvider>
    );
    localStorage.setItem('app:language', 'en');
    const view = render(ui(true));
    const signal = get.mock.calls[0][0] as AbortSignal;
    view.rerender(ui(false));
    expect(signal.aborted).toBe(true);
    expect(off).toHaveBeenCalledOnce();
    view.rerender(ui(true));
    expect(await screen.findByRole('option', { name: 'Technology (3)' })).toBeVisible();
    await act(async () => resolve({ ...facets, category: [{ value: 'technology', count: 99 }] }));
    expect(screen.queryByRole('option', { name: 'Technology (99)' })).toBeNull();
    view.unmount();
    expect(off).toHaveBeenCalledTimes(2);
  });
  it('adapts scope and fallback tags, registers only supported AI capabilities, and shares facet events', async () => {
    const http = fakeHttp();
    vi.mocked(http.send).mockResolvedValue(
      new Response(JSON.stringify({ tags: [], reason: 'provider_offline' })),
    );
    const result = await createSuggestApi(http).suggest('lamp', { scope: 'web' });
    expect(result.tags).toEqual([]);
    expect(http.send).toHaveBeenCalledWith('POST', '/api/v1/search/suggest', {
      q: 'lamp',
      scope: 'sites',
    });
    const off = vi.fn();
    const events = { on: vi.fn(() => off), state: 'open' as const, lastEventId: null };
    const api = createFacetsApi(http, events);
    const controller = new AbortController();
    await api.get(controller.signal);
    expect(http.get).toHaveBeenCalledWith('/api/v1/facets', undefined, controller.signal);
    api.onChanged(vi.fn())();
    expect(off).toHaveBeenCalledTimes(2);
    const client = createHttpClient(http, {
      me: { ...OWNER, capabilities: { ...OWNER.capabilities, 'ai.tasks': true } },
      events,
    });
    expect(client.ai?.suggest).toBeDefined();
    expect(client.ai?.tags).toBeDefined();
    expect(client.ai?.webQueue).toBeDefined();
    expect(client.capabilities.aiSuggest).toBe(true);
    expect(createHttpClient(http, { me: OWNER, events }).ai?.suggest).toBeUndefined();
  });
});
