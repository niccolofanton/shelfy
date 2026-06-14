import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import AiTags from '../../src/views/AiTags';

// A populated overview — `analyzed > 0` is what flips the view out of the
// empty-state and into the full dashboard.
const FULL_OVERVIEW = {
  total: 100,
  analyzed: 80,
  unanalyzed: 20,
  uniqueTags: 12,
  taggedPosts: 60,
  byCategory: [
    { category: 'Food', count: 30 },
    { category: 'Travel', count: 20 },
  ],
  byContentType: [{ contentType: 'Reel', count: 25 }],
  languages: [],
};

const TAG_STATS = [
  { tag: 'sunset', count: 18, lastUsed: '2026-05-01T00:00:00Z' },
  { tag: 'pasta', count: 9, lastUsed: '2026-04-01T00:00:00Z' },
];

const CLUSTERS = [{ id: 'c1', topTag: 'food', postCount: 40, tags: ['pasta', 'pizza', 'recipe'] }];

const ENTITIES = [
  { entity: 'Apple', count: 7 },
  { entity: 'Nike', count: 4 },
];

// The tag cloud was removed; single-tag selection now happens via the "Tag da
// sistemare" (health) orphan chips, which call addTag(tag) like the cloud did.
const HEALTH = {
  orphanTags: [
    { tag: 'sunset', count: 1 },
    { tag: 'pasta', count: 1 },
  ],
  rareTags: 2,
  unanalyzedPosts: 20,
  untaggedPosts: 0,
};

function populate() {
  window.electronAPI.getAiOverview.mockResolvedValue(FULL_OVERVIEW);
  window.electronAPI.getTagStats.mockResolvedValue(TAG_STATS);
  window.electronAPI.getTagClusters.mockResolvedValue(CLUSTERS);
  window.electronAPI.getEntityStats.mockResolvedValue(ENTITIES);
  window.electronAPI.getTagHealth.mockResolvedValue(HEALTH);
}

describe('AiTags — empty state', () => {
  it('renders the empty-state CTA when nothing has been analyzed', async () => {
    // Default overview from setup.js has analyzed: 0 → empty state.
    render(<AiTags />);
    await waitFor(() => expect(screen.getByTestId('empty-analyze-btn')).toBeInTheDocument());
    expect(screen.getByText('Nessun post analizzato')).toBeInTheDocument();
  });

  it('the empty-state button triggers analyzeMissing', async () => {
    render(<AiTags />);
    const btn = await screen.findByTestId('empty-analyze-btn');
    fireEvent.click(btn);
    await waitFor(() => expect(window.electronAPI.analyzeMissing).toHaveBeenCalled());
  });
});

describe('AiTags — dashboard', () => {
  beforeEach(populate);

  it('loads all the aggregate data on mount', async () => {
    render(<AiTags />);
    await waitFor(() => expect(screen.getByTestId('aitags-dashboard')).toBeInTheDocument());
    expect(window.electronAPI.getAiOverview).toHaveBeenCalled();
    expect(window.electronAPI.getTagStats).toHaveBeenCalled();
    expect(window.electronAPI.getTagClusters).toHaveBeenCalled();
    expect(window.electronAPI.getEntityStats).toHaveBeenCalled();
  });

  it('renders the dashboard, clusters and entities (no tag cloud)', async () => {
    render(<AiTags />);
    await waitFor(() => expect(screen.getByTestId('aitags-dashboard')).toBeInTheDocument());

    // Cluster
    expect(screen.getByText('food')).toBeInTheDocument();
    // Entities
    expect(screen.getByText('Apple')).toBeInTheDocument();
    expect(screen.getByText('Nike')).toBeInTheDocument();
    // Health orphan chips replace the old cloud as the single-tag entry point
    expect(screen.getByRole('button', { name: 'sunset' })).toBeInTheDocument();
    // Dashboard coverage numbers + analyze-missing CTA
    expect(screen.getByTestId('analyze-missing-btn')).toBeInTheDocument();
  });

  it('clicking an orphan tag fetches posts filtered by that tag', async () => {
    window.electronAPI.getPosts.mockResolvedValue({ posts: [], total: 3 });
    render(<AiTags />);
    await waitFor(() => expect(screen.getByTestId('aitags-dashboard')).toBeInTheDocument());

    fireEvent.click(screen.getByRole('button', { name: 'sunset' }));
    await waitFor(() =>
      expect(window.electronAPI.getPosts).toHaveBeenCalledWith(
        expect.objectContaining({ tags: ['sunset'], tagMode: 'or' }),
      ),
    );
  });

  it('clicking a cluster fetches posts for all the cluster tags in OR mode', async () => {
    window.electronAPI.getPosts.mockResolvedValue({ posts: [], total: 5 });
    render(<AiTags />);
    await waitFor(() => expect(screen.getByText('food')).toBeInTheDocument());

    fireEvent.click(screen.getByText('food'));
    await waitFor(() =>
      expect(window.electronAPI.getPosts).toHaveBeenCalledWith(
        expect.objectContaining({ tags: ['pasta', 'pizza', 'recipe'], tagMode: 'or' }),
      ),
    );
  });

  it('selecting a tag shows the result count from the fetch', async () => {
    window.electronAPI.getPosts.mockResolvedValue({ posts: [], total: 42 });
    render(<AiTags />);
    await waitFor(() => expect(screen.getByTestId('aitags-dashboard')).toBeInTheDocument());

    fireEvent.click(screen.getByRole('button', { name: 'pasta' }));
    await waitFor(() =>
      expect(screen.getByTestId('result-count')).toHaveTextContent('42 risultati'),
    );
    // 'pasta' is the orphan health chip (the cloud was removed).
  });
});
