// Synthetic browser API contract checks: no account or inference is used.
import { test, expect, apiPost, OWNER } from './api';
import type { Page } from '@playwright/test';

async function fixture(page: Page, enabled = true) {
  const posts = [
    apiPost({
      key: 'ig_1',
      caption: 'Lamp study',
      aiTags: ['glass', 'lighting'],
      aiCategory: 'technology',
      aiContentType: 'portfolio',
      aiLanguage: 'en',
      aiStatus: 'done',
    }),
    apiPost({
      key: 'x_2',
      platform: 'twitter',
      caption: 'Lighting guide',
      aiTags: ['lighting'],
      aiCategory: 'technology',
      aiContentType: 'docs',
      aiLanguage: 'it',
      aiStatus: 'done',
    }),
    apiPost({
      key: 'pin_3',
      platform: 'pinterest',
      caption: 'Kitchen shelves',
      aiTags: ['kitchen'],
      aiCategory: 'food-beverage',
      aiContentType: 'portfolio',
      aiLanguage: 'it',
    }),
    apiPost({
      key: 'ig_trashed',
      caption: 'Lamp deleted',
      aiLanguage: 'fr',
      deletedAt: Date.now(),
    }),
  ];
  const suggestions: { q: string; scope: string }[] = [];
  const reads: URLSearchParams[] = [];
  const preference = { enabled };
  await page.route('**/api/v1/me', (route) =>
    route.fulfill({
      json: { ...OWNER, capabilities: { ...OWNER.capabilities, 'ai.tasks': true } },
    }),
  );
  await page.route('**/api/v1/me/settings', (route) => {
    if (route.request().method() === 'PUT')
      preference.enabled = !!route.request().postDataJSON().aiSuggestions;
    return route.fulfill({
      json: {
        language: null,
        archiveAssetTypes: { thumbnail: true, image: true, video: true },
        aiRouting: {},
        aiConcurrency: 1,
        aiSuggestions: preference.enabled,
        aiVisionQc: false,
        aiAutoAnalyzeWebsites: false,
        aiDictationInterim: false,
      },
    });
  });
  await page.route('**/api/v1/me/providers', (route) => route.fulfill({ json: [] }));
  await page.route('**/api/v1/me/usage/ai*', (route) => route.fulfill({ json: { days: [] } }));
  await page.route('**/api/v1/ai/queue*', (route) =>
    route.fulfill({
      json: {
        items: [],
        cursor: null,
        paused: false,
        providerState: null,
        etaMs: null,
        counts: { unanalyzed: 1, pending: 0, analyzing: 0, done: 2, error: 0 },
      },
    }),
  );
  await page.route('**/api/v1/search/suggest', (route) => {
    suggestions.push(route.request().postDataJSON());
    return route.fulfill({ json: { tags: ['glass', 'lighting'] } });
  });
  await page.route('**/api/v1/facets', (route) => {
    const values = (
      field: 'aiCategory' | 'aiContentType' | 'aiStatus' | 'aiLanguage',
      none = false,
    ) => {
      const counts = new Map<string, number>();
      for (const post of posts.filter((post) => post.deletedAt == null)) {
        const value = post[field] ?? (none ? 'none' : null);
        if (value) counts.set(value, (counts.get(value) ?? 0) + 1);
      }
      return [...counts].map(([value, count]) => ({ value, count }));
    };
    return route.fulfill({
      json: {
        category: values('aiCategory'),
        contentType: values('aiContentType'),
        status: values('aiStatus', true),
        language: values('aiLanguage'),
      },
    });
  });
  const matching = (query: URLSearchParams) =>
    posts.filter((post) => {
      if (post.deletedAt != null) return false;
      for (const [param, field] of [
        ['category', 'aiCategory'],
        ['contentType', 'aiContentType'],
        ['aiLanguage', 'aiLanguage'],
      ] as const)
        if (query.has(param) && query.get(param) !== post[field]) return false;
      if (query.has('aiStatus') && query.get('aiStatus') !== (post.aiStatus ?? 'none'))
        return false;
      const text = `${post.caption} ${post.aiTags.join(' ')}`.toLowerCase();
      const terms = [query.get('q'), ...query.getAll('concept')].filter(
        (term): term is string => !!term,
      );
      return (
        !terms.length ||
        (query.get('conceptMode') === 'and'
          ? terms.every((term) => text.includes(term.toLowerCase()))
          : terms.some((term) => text.includes(term.toLowerCase())))
      );
    });
  await page.route('**/api/v1/posts?*', async (route) => {
    const query = new URL(route.request().url()).searchParams;
    reads.push(query);
    // Let the real search transition hold the previous grid during fetching.
    await new Promise((resolve) => setTimeout(resolve, 80));
    const items = matching(query);
    await route.fulfill({ json: { items, nextCursor: null, total: items.length } });
  });
  await page.route('**/api/v1/posts/count*', (route) =>
    route.fulfill({
      json: { total: matching(new URL(route.request().url()).searchParams).length },
    }),
  );
  return { suggestions, reads };
}

test('opt-in suggestions widen the grid with OR then narrow with AND; live facets filter and count', async ({
  page,
  api,
}) => {
  const h = await fixture(page);
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByRole('searchbox', { name: 'Search posts' }).fill('lamp');
  await expect(page.getByTestId('suggested-tag')).toHaveCount(2);
  expect(h.suggestions).toEqual([{ q: 'lamp', scope: 'all' }]);
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  await page.getByTestId('suggested-tag').filter({ hasText: 'lighting' }).click();
  await expect(page.getByTestId('post-card')).toHaveCount(2);
  await page.getByTestId('suggested-tag').filter({ hasText: 'glass' }).click();
  await page.getByTestId('concept-mode-and').click();
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  expect(
    h.reads.some(
      (query) => query.get('conceptMode') === 'and' && query.getAll('concept').length === 2,
    ),
  ).toBe(true);
  await page.getByRole('button', { name: 'Clear search', exact: true }).click();
  await expect(page.getByTestId('suggested-tags-bar')).toHaveCount(0);
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByTestId('filters-toggle').click();
  await expect(
    page.getByTestId('drawer-category-select').getByRole('option', { name: 'Technology (2)' }),
  ).toHaveCount(1);
  await expect(
    page.getByTestId('drawer-contenttype-select').getByRole('option', { name: 'Portfolio (2)' }),
  ).toHaveCount(1);
  await expect(
    page.getByTestId('drawer-aistatus-select').getByRole('option', { name: 'Unanalyzed (1)' }),
  ).toHaveCount(1);
  await expect(
    page.getByTestId('drawer-language-select').getByRole('option', { name: 'Italian (2)' }),
  ).toHaveCount(1);
  await expect(
    page.getByTestId('drawer-language-select').getByRole('option', { name: 'French (1)' }),
  ).toHaveCount(0);
  if (process.env.SHELFY_E2E_SHOTS)
    await page.screenshot({ path: `${process.env.SHELFY_E2E_SHOTS}/ai-gallery-facets.png` });
  await page.getByTestId('drawer-language-select').selectOption('it');
  await expect(page.getByTestId('post-card')).toHaveCount(2);
  await page.getByTestId('drawer-aistatus-select').selectOption('none');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  await page.getByTestId('drawer-reset').click();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  expect(api.thirdParty).toEqual([]);
});

test('disabled account preference makes no suggestion request, even with the desktop local opt-in', async ({
  page,
  api,
}) => {
  const h = await fixture(page, false);
  await page.addInitScript(() => localStorage.setItem('aiSearchSuggestions', 'true'));
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByRole('searchbox', { name: 'Search posts' }).fill('lamp');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  // Advance browser time beyond both debounces without making an inference.
  await page.waitForTimeout(1000);
  expect(h.suggestions).toEqual([]);
  await expect(page.getByTestId('suggested-tags-bar')).toHaveCount(0);
  expect(api.thirdParty).toEqual([]);
});

test('live language facet remains usable in the narrow filter sheet', async ({ page, api }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await fixture(page);
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByTestId('filters-toggle').click();
  const drawer = page.getByTestId('filter-drawer');
  await expect(drawer).toHaveAttribute('role', 'dialog');
  await page.getByTestId('drawer-language-select').selectOption('en');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  await expect(page.getByTestId('drawer-language-select')).toHaveValue('en');
  await page.getByTestId('drawer-reset').click();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  expect(api.thirdParty).toEqual([]);
});
