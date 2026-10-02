import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import AiWebsites from '../../src/views/AiWebsites';
import { streamPreview, toWebJob } from '../../src/views/websites/model';

// The Websites view is a design-reference browser over queryWebReferences /
// getWebFacets, with a live queue (incl. the anti-bot "pass the check" flow)
// and a tabbed detail panel. These tests pin the contracts with the IPC layer
// and the defensive rendering of loosely-typed job payloads.

const api = window.electronAPI as unknown as Record<string, Mock>;

function makePost(over: Partial<Shelfy.Post> = {}): Shelfy.Post {
  return {
    id: 'web:linear',
    platform: 'web',
    authorName: 'The system for product development',
    webUrl: 'https://linear.app',
    webFinalUrl: 'https://linear.app/',
    webDomain: 'linear.app',
    postUrl: 'https://linear.app/',
    webCapturedAt: 1_790_934_813,
    webPalette: [
      { hex: '#08090a', name: 'black', role: 'background', coverage: 0.6 },
      { hex: '#5e6ad2', name: 'vivid indigo', role: 'accent', coverage: 0.05 },
    ],
    webFonts: [{ family: 'Inter', role: 'display', classification: 'sans', weights: [400, 510] }],
    webTech: ['React'],
    webAwards: [],
    webPages: [
      {
        url: 'https://linear.app/',
        pageType: 'home',
        title: 'Linear',
        hero: { path: '/data/assets/web/hero.webp', width: 2880, height: 1800 },
        chunks: [
          {
            screenshotPath: '/data/assets/web/c0.webp',
            width: 2880,
            height: 4000,
            top: 0,
            cssHeight: 2000,
          },
        ],
        footer: { path: '/data/assets/web/footer.webp', width: 2880, height: 1800 },
        sections: [
          {
            kind: 'pricing',
            heading: 'Plans for every team',
            top: 900,
            cssHeight: 700,
            path: '/data/assets/web/s1.webp',
            width: 2880,
            height: 1400,
          },
        ],
      },
    ],
    webMeta: {
      siteName: 'Linear',
      favicon: '/data/assets/web/fav.webp',
      scheme: 'dark',
      contrast: { text: '#dde3fa', background: '#07090a', ratio: 15.6 },
      traits: { fixedHeader: true, glass: true },
      tech: [{ name: 'React', category: 'ui-library', confidence: 0.9 }],
      video: {
        path: '/data/v.mp4',
        preview: '/data/p.mp4',
        poster: null,
        width: 1280,
        height: 800,
        duration: 12,
      },
    },
    aiWeb: {
      schema: 2,
      observations: 'Fixed dark header.',
      siteType: 'saas',
      industry: 'developer-tools',
      audience: 'Product teams',
      style: ['minimal'],
      theme: 'dark',
      colorMood: ['monochrome'],
      density: 'balanced',
      layoutPatterns: ['split hero'],
      heroType: 'product-shot',
      imagery: ['ui-screenshots'],
      typography: ['geometric sans'],
      components: ['pricing table'],
      craft: 'exceptional',
      notableDetails: ['Split testimonial cards.'],
      referenceFor: ['borrow split-color testimonial cards'],
      summary: 'A high-contrast dark SaaS reference.',
      description: 'Inter headline over a black background.',
      tags: [],
      searchKeywords: [],
      language: 'en',
      facets: { siteType: ['saas'], style: ['minimal'], tech: ['react'], font: ['inter'] },
    },
    aiTags: [],
    aiEntities: [],
    aiKeywords: [],
    ...over,
  } as unknown as Shelfy.Post;
}

beforeEach(() => {
  api.queryWebReferences = vi.fn().mockResolvedValue({ posts: [makePost()], total: 1 });
  api.getWebFacets = vi.fn().mockResolvedValue({
    siteType: [{ value: 'saas', count: 2 }],
    style: [{ value: 'minimal', count: 3 }],
  });
  api.getWebSnapshots = vi.fn().mockResolvedValue([]);
  api.getSimilarWebReferences = vi.fn().mockResolvedValue([]);
  api.unblockWebJob = vi.fn().mockResolvedValue({ ok: true });
  api.recatalogWebReferences = vi.fn().mockResolvedValue({ queued: 2 });
  api.addWebReference = vi.fn().mockResolvedValue({ id: 'web:new' });
  api.getPostsByIds = vi.fn().mockResolvedValue([]);
  api.openExternal = vi.fn();
});

describe('AiWebsites library', () => {
  it('renders site cards with localized catalog labels and the facet panel', async () => {
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    const card = await screen.findByTestId('aiweb-card');
    expect(within(card).getByText('Linear')).toBeInTheDocument();
    expect(within(card).getByText('Strumenti per sviluppatori')).toBeInTheDocument();
    expect(within(card).getByTestId('aiweb-palette-strip')).toBeInTheDocument();
    const facets = await screen.findByTestId('aiweb-facets');
    expect(within(facets).getByText('Tipo di sito')).toBeInTheDocument();
    expect(within(facets).getByText('Minimal')).toBeInTheDocument();
  });

  it('queries with the selected facet values (OR within, AND across)', async () => {
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    const facets = await screen.findByTestId('aiweb-facets');
    fireEvent.click(within(facets).getByText('Minimal'));
    await waitFor(() =>
      expect(api.queryWebReferences).toHaveBeenLastCalledWith(
        expect.objectContaining({ facets: { style: ['minimal'] }, offset: 0 }),
      ),
    );
    expect(await screen.findByTestId('aiweb-active-chip')).toHaveTextContent('Minimal');
  });

  it('re-catalogs outdated sites and reports the result', async () => {
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    fireEvent.click(await screen.findByTestId('aiweb-recatalog'));
    await waitFor(() =>
      expect(api.recatalogWebReferences).toHaveBeenCalledWith({ outdatedOnly: true }),
    );
    expect(await screen.findByTestId('aiweb-toast')).toHaveTextContent('2 siti in coda');
  });
});

describe('AiWebsites live queue', () => {
  it('offers "Supera la verifica" for a blocked job and renders object payloads safely', async () => {
    const blocked = {
      key: 'web:web:linear',
      postId: 'web:linear',
      status: 'blocked',
      domain: 'linear.app',
      blocked: { vendor: 'cloudflare', url: 'https://linear.app/', reason: 'HTTP 403' },
      // A stage/event carrying objects must never reach React as a child.
      stage: { nested: true },
      events: [{ id: 1, ts: 1, kind: 'error', text: 'Blocked', data: { vendor: { x: 1 } } }],
    };
    render(<AiWebsites webJobs={{ jobs: [blocked] }} />);
    const queue = await screen.findByTestId('aiweb-queue');
    expect(within(queue).getByText(/cloudflare/)).toBeInTheDocument();
    fireEvent.click(within(queue).getByTestId('aiweb-unblock'));
    await waitFor(() => expect(api.unblockWebJob).toHaveBeenCalledWith('web:web:linear'));
  });

  it('normalises job records to strings only', () => {
    const job = toWebJob({
      postId: 'p',
      status: 'weird',
      stage: { a: 1 },
      events: [{ text: { a: 1 } }],
    });
    expect(job?.status).toBe('pending');
    expect(job?.stage).toBe('');
    expect(job?.events).toEqual([]);
  });

  it('previews the v2 catalog stream (summary, else observations)', () => {
    expect(
      streamPreview('{"observations":"Dark hero","site_type":"saas","summary":"A dark SaaS'),
    ).toEqual({
      key: 'summary',
      text: 'A dark SaaS',
    });
    expect(streamPreview('{"observations":"Dark he').key).toBe('observations');
    expect(streamPreview('{"tags":[').key).toBe('raw');
  });
});

describe('AiWebsites detail', () => {
  it('opens a site with its tabs and closes on Escape', async () => {
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    const card = await screen.findByTestId('aiweb-card');
    fireEvent.click(within(card).getByRole('button', { name: 'Linear' }));
    const detail = await screen.findByTestId('aiweb-detail');
    expect(within(detail).getByTestId('aiweb-summary')).toHaveTextContent(
      'high-contrast dark SaaS',
    );
    expect(within(detail).getByText('Da riutilizzare')).toBeInTheDocument();

    fireEvent.click(within(detail).getByTestId('aiweb-tab-sections-btn'));
    expect(await within(detail).findByText('Plans for every team')).toBeInTheDocument();

    fireEvent.click(within(detail).getByTestId('aiweb-tab-design-btn'));
    expect(within(detail).getByTestId('aiweb-contrast')).toHaveTextContent('15.6:1');
    expect(within(detail).getByText('Header fisso')).toBeInTheDocument();

    fireEvent.click(within(detail).getByTestId('aiweb-tab-similar-btn'));
    await waitFor(() => expect(api.getSimilarWebReferences).toHaveBeenCalledWith('web:linear', 12));

    act(() => {
      fireEvent.keyDown(window, { key: 'Escape' });
    });
    await waitFor(() => expect(screen.queryByTestId('aiweb-detail')).not.toBeInTheDocument());
  });

  it('applies a facet from the overview chips', async () => {
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    fireEvent.click(
      within(await screen.findByTestId('aiweb-card')).getByRole('button', { name: 'Linear' }),
    );
    const chips = within(await screen.findByTestId('aiweb-overview-facets')).getAllByTestId(
      'aiweb-facet-chip',
    );
    fireEvent.click(chips.find((c) => c.textContent === 'Minimal') as HTMLElement);
    await waitFor(() => expect(screen.queryByTestId('aiweb-detail')).not.toBeInTheDocument());
    await waitFor(() =>
      expect(api.queryWebReferences).toHaveBeenLastCalledWith(
        expect.objectContaining({ facets: { style: ['minimal'] } }),
      ),
    );
  });

  it('degrades gracefully for a v1 row (no catalog, no sections)', async () => {
    api.queryWebReferences = vi.fn().mockResolvedValue({
      posts: [
        makePost({
          aiWeb: null,
          aiDescription: 'Legacy description',
          aiTags: ['dark'],
          webMeta: null,
          webPalette: ['#000000' as unknown as Shelfy.WebSwatch],
          webPages: [{ url: 'https://linear.app/', screenshotPath: '/data/old.webp' }],
        }),
      ],
      total: 1,
    });
    render(<AiWebsites webJobs={{ jobs: [] }} />);
    fireEvent.click(within(await screen.findByTestId('aiweb-card')).getByRole('button'));
    const detail = await screen.findByTestId('aiweb-detail');
    expect(within(detail).getByText('Legacy description')).toBeInTheDocument();
    fireEvent.click(within(detail).getByTestId('aiweb-tab-sections-btn'));
    expect(within(detail).getByText(/Nessuna sezione rilevata/)).toBeInTheDocument();
  });
});
