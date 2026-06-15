import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import type { ComponentProps } from 'react';
import Sidebar from '../../src/components/Sidebar';
import { ActivityProvider } from '../../src/hooks/useActivity';

const defaultStats = {
  total: 100,
  byPlatform: { instagram: 60, twitter: 40 },
  byMediaType: {},
  downloaded: 10,
};

type SidebarProps = ComponentProps<typeof Sidebar>;
// `setup` overrides may pass `stats: null` (the "no stats provided" case the
// component tolerates at runtime), which the prop type models as optional rather
// than nullable — so the override bag widens `stats` accordingly.
type SetupProps = Partial<Omit<SidebarProps, 'stats'>> & { stats?: SidebarProps['stats'] | null };

// Sidebar embeds <ActivityCenter>, which consumes the activity context — wrap the
// render so the footer strip mounts the same way it does inside the app.
function setup(props: SetupProps = {}) {
  const onNavigate = vi.fn();
  const onSelectBrowserTab = vi.fn();
  const result = render(
    <ActivityProvider>
      <Sidebar
        currentView="gallery"
        onNavigate={onNavigate}
        stats={defaultStats}
        newPostsAlert={{}}
        browserSyncing={{}}
        onSelectBrowserTab={onSelectBrowserTab}
        {...(props as Partial<SidebarProps>)}
      />
    </ActivityProvider>,
  );
  return { onNavigate, onSelectBrowserTab, ...result };
}

describe('Sidebar', () => {
  describe('rendering', () => {
    it('renders the main nav groups and items', () => {
      setup();
      // Connections + Library are collapsible group headers; their sub-rows navigate.
      // (testids keep the legacy browser/bookmarks names for persistence compat.)
      expect(screen.getByTestId('nav-browser')).toHaveTextContent('Connessioni');
      expect(screen.getByTestId('nav-bookmarks')).toHaveTextContent('Libreria');
      expect(screen.getByTestId('nav-ai')).toBeInTheDocument();
      expect(screen.getByTestId('browser-tab-instagram')).toBeInTheDocument();
      expect(screen.getByTestId('browser-tab-twitter')).toBeInTheDocument();
      expect(screen.getByTestId('nav-downloads')).toBeInTheDocument();
    });

    it('shows total post count in the header', () => {
      setup({ stats: { ...defaultStats, total: 250 } });
      expect(screen.getByText('250 post')).toBeInTheDocument();
    });

    it('shows platform stats for Instagram', () => {
      setup();
      const row = screen.getByTestId('source-instagram');
      expect(within(row).getByText('Instagram')).toBeInTheDocument();
      expect(within(row).getByText('60')).toBeInTheDocument();
    });

    it('shows platform stats for X / Twitter', () => {
      setup();
      const row = screen.getByTestId('source-twitter');
      expect(within(row).getByText('X / Twitter')).toBeInTheDocument();
      expect(within(row).getByText('40')).toBeInTheDocument();
    });

    it('shows 0 post count when stats are not provided', () => {
      setup({ stats: null });
      expect(screen.getByText('0 post')).toBeInTheDocument();
    });
  });

  describe('navigation clicks', () => {
    it('clicking "Tutti i post" selects the all-posts source', () => {
      const onSelectSource = vi.fn();
      setup({ onSelectSource });
      fireEvent.click(screen.getByTestId('source-all'));
      expect(onSelectSource).toHaveBeenCalledWith({ type: 'platform', value: 'all' });
    });

    it('clicking Downloads nav button calls onNavigate("downloads")', () => {
      const { onNavigate } = setup();
      fireEvent.click(screen.getByRole('button', { name: /downloads/i }));
      expect(onNavigate).toHaveBeenCalledWith('downloads');
    });

    it('clicking a browser sub-tab calls onSelectBrowserTab with its id', () => {
      const { onSelectBrowserTab } = setup();
      fireEvent.click(screen.getByTestId('browser-tab-twitter'));
      expect(onSelectBrowserTab).toHaveBeenCalledWith('twitter');
    });
  });

  describe('action rows (open modals, not views)', () => {
    it('labels the three add actions with verb-first copy', () => {
      setup();
      expect(screen.getByTestId('browser-tab-add-site')).toHaveTextContent('Aggiungi sito');
      expect(screen.getByTestId('browser-tab-add-bookmark')).toHaveTextContent('Aggiungi file');
      expect(screen.getByTestId('add-source-btn')).toHaveTextContent('Nuova cartella');
    });

    it('styles the action rows lighter than nav rows (text-gray-500)', () => {
      setup();
      for (const id of ['browser-tab-add-site', 'browser-tab-add-bookmark', 'add-source-btn']) {
        expect(screen.getByTestId(id).className).toContain('text-gray-500');
      }
      // Nav destinations keep the regular gray-400 tone.
      expect(screen.getByTestId('browser-tab-instagram').className).toContain('text-gray-400');
    });

    it('clicking the add-site row calls onAddSite', () => {
      const onAddSite = vi.fn();
      setup({ onAddSite });
      fireEvent.click(screen.getByTestId('browser-tab-add-site'));
      expect(onAddSite).toHaveBeenCalledTimes(1);
    });

    it('clicking the add-bookmark row calls onAddBookmark', () => {
      const onAddBookmark = vi.fn();
      setup({ onAddBookmark });
      fireEvent.click(screen.getByTestId('browser-tab-add-bookmark'));
      expect(onAddBookmark).toHaveBeenCalledTimes(1);
    });
  });

  describe('newPostsAlert badge (per sub-tab)', () => {
    it('does not show any badge when counts are empty', () => {
      const { container } = setup({ newPostsAlert: {} });
      expect(container.querySelector('nav .bg-blue-600')).toBeNull();
    });

    it('shows the count on the matching sub-tab only', () => {
      setup({ newPostsAlert: { instagram: 5 } });
      const igTab = screen.getByTestId('browser-tab-instagram');
      expect(within(igTab).getByText('5')).toBeInTheDocument();
      const twTab = screen.getByTestId('browser-tab-twitter');
      expect(within(twTab).queryByTestId('browser-tab-twitter-badge')).toBeNull();
    });

    it('shows the exact number up to five digits', () => {
      setup({ newPostsAlert: { instagram: 12345 } });
      expect(screen.getByText('12345')).toBeInTheDocument();
    });

    it('caps at "99999+" beyond five digits', () => {
      setup({ newPostsAlert: { twitter: 123456 } });
      expect(screen.getByText('99999+')).toBeInTheDocument();
    });

    it('shows the exact boundary value 99999', () => {
      setup({ newPostsAlert: { twitter: 99999 } });
      expect(screen.getByText('99999')).toBeInTheDocument();
    });
  });

  describe('sync indicator (per sub-tab)', () => {
    it('shows the spinning indicator on a syncing sub-tab', () => {
      setup({ browserSyncing: { instagram: true } });
      expect(screen.getByTestId('browser-tab-instagram-syncing')).toBeInTheDocument();
      expect(screen.queryByTestId('browser-tab-twitter-syncing')).toBeNull();
    });

    it('shows no indicator when nothing is syncing', () => {
      setup({ browserSyncing: {} });
      expect(screen.queryByTestId('browser-tab-instagram-syncing')).toBeNull();
      expect(screen.queryByTestId('browser-tab-twitter-syncing')).toBeNull();
    });
  });

  describe('custom sources (collections)', () => {
    const collections: Shelfy.Collection[] = [
      { id: 1, name: 'Ricette', color: '#e91e63', count: 12 },
      { id: 2, name: 'Viaggi', color: '#4caf50', count: 7 },
    ] as unknown as Shelfy.Collection[];

    it('renders the add-source button and calls onAddCollection on click', () => {
      const onAddCollection = vi.fn();
      setup({ onAddCollection });
      fireEvent.click(screen.getByTestId('add-source-btn'));
      expect(onAddCollection).toHaveBeenCalledTimes(1);
    });

    it('renders each custom collection with its name and count', () => {
      setup({ collections });
      expect(screen.getByText('Ricette')).toBeInTheDocument();
      expect(screen.getByText('Viaggi')).toBeInTheDocument();
      expect(screen.getByText('12')).toBeInTheDocument();
      expect(screen.getByText('7')).toBeInTheDocument();
    });

    it('clicking a collection selects it as a collection source', () => {
      const onSelectSource = vi.fn();
      setup({ collections, onSelectSource });
      fireEvent.click(screen.getByTestId('source-collection-1'));
      expect(onSelectSource).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'collection', value: 1 }),
      );
    });

    it('clicking a platform source selects it as a platform source', () => {
      const onSelectSource = vi.fn();
      setup({ onSelectSource });
      fireEvent.click(screen.getByTestId('source-instagram'));
      expect(onSelectSource).toHaveBeenCalledWith({ type: 'platform', value: 'instagram' });
    });

    it('clicking the edit button calls onEditCollection with that collection', () => {
      const onEditCollection = vi.fn();
      setup({ collections, onEditCollection });
      fireEvent.click(screen.getByTestId('edit-collection-2'));
      expect(onEditCollection).toHaveBeenCalledWith(
        expect.objectContaining({ id: 2, name: 'Viaggi' }),
      );
    });
  });

  // The rich background-activity surface (strip + popover) moved to <ActivityCenter>
  // (see ActivityCenter.test.tsx). The sidebar keeps only the at-a-glance badge that
  // rides on the "AI Tags" nav row, driven by the analysis* summary props.
  describe('AI Tags nav badge', () => {
    it('does not show the nav badge when idle', () => {
      setup({ analysisActive: false });
      expect(screen.queryByTestId('nav-aiqueue-badge')).toBeNull();
    });

    it('shows the done/total badge on the AI Tags nav when active', () => {
      setup({ analysisActive: true, analysisDone: 2, analysisTotal: 5 });
      expect(screen.getByTestId('nav-aiqueue-badge')).toHaveTextContent('2/5');
      expect(screen.getByTestId('nav-aiqueue-analyzing')).toBeInTheDocument();
    });
  });

  describe('active nav item', () => {
    it('active nav item renders with bg-[#1e1e1e] class', () => {
      setup({ currentView: 'settings' });
      expect(screen.getByTestId('nav-settings').className).toContain('bg-[#1e1e1e]');
    });

    it('inactive nav item does not have bg-[#1e1e1e] class', () => {
      setup({ currentView: 'settings' });
      expect(screen.getByTestId('nav-downloads').className).not.toContain('bg-[#1e1e1e]');
    });

    it('the active browser sub-tab is highlighted', () => {
      setup({ currentView: 'browser', browserTab: 'twitter' });
      expect(screen.getByTestId('browser-tab-twitter').className).toContain('bg-[#1e1e1e]');
      expect(screen.getByTestId('browser-tab-instagram').className).not.toContain('bg-[#1e1e1e]');
    });

    it('Downloads is active when currentView is "downloads"', () => {
      setup({ currentView: 'downloads' });
      const downloadsBtn = screen.getByRole('button', { name: /downloads/i });
      expect(downloadsBtn.className).toContain('bg-[#1e1e1e]');
    });
  });
});
