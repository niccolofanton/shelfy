// UX-0 (docs/web-port/reviews/ux-audit.md §5): the visual and accessibility
// harness. On a real server it signs the synth account (a library of real
// posts) in, captures every §2 screen at the audit's four viewports into
// SHELFY_E2E_SHOTS (`ux-<screen>-<viewport>.png`) and asserts, per viewport:
//
//   - no horizontal overflow on the phones, also after focusing each toolbar
//     control (nothing may scroll the column sideways);
//   - the listed controls sit inside the viewport and are at least 44×44 on
//     the phones (WCAG 2.5.5 / the audit's one touch-target rule);
//   - the post modal is on top (`elementFromPoint` at its header resolves
//     inside `post-modal`), also over the shell's menu button;
//   - a keyboard Tab lands on a control that paints a visible focus ring;
//   - every visible text node of the key screens keeps WCAG AA contrast (4.5:1).
//
// A defect the audit lists and a later lane fixes is not hidden: it is in
// KNOWN below, with its finding ID. The check still runs. A known failure is
// reported (an annotation, and a line in the output) and does not fail the
// suite; a known failure that has stopped failing DOES fail it ("fixed:
// remove it from KNOWN"), so each lane deletes exactly its own entries.
//
//   SHELFY_E2E_SHOTS=/some/dir pnpm run test:e2e:web:server -- ux.spec.ts
import { test, expect, type Browser, type BrowserContext, type Page } from '@playwright/test';
import { E2E } from './env';
import {
  UX_VIEWPORTS,
  lowContrastText,
  hitReport,
  newContext,
  newViewportContext,
  overflowReport,
  shot,
  signedInState,
  tabToFocusRing,
  topmostTestIds,
  type UxViewport,
} from './support';

// `<check>:<viewport>:<subject>` → the audit finding (or note) behind it.
// Filled from the first run on the current tip; a lane removes its entries
// when it enables its own check.
const ALL = ['desktop', 'tablet', 'ios', 'android'];
const PHONES = ['ios', 'android'];
const KNOWN: Record<string, string> = Object.fromEntries(
  (
    [
      // Contrast (WCAG AA 4.5:1), every viewport.
      [
        'contrast',
        ALL,
        'post-modal/[post-modal-meta] div',
        'UX-5 MOD: "Your tags & note" label (4.4:1)',
      ],
      [
        'contrast',
        ALL,
        'post-modal/[post-modal-meta] span',
        'UX-5 MOD: "Your tags" and "Note" labels (3.5:1)',
      ],
      [
        'contrast',
        ALL,
        'post-modal/[post-modal-note-add] button',
        'UX-5 MOD: "Add a personal note" (4.4:1)',
      ],
      ['contrast', ALL, 'trash/[trash-empty-state] p', 'UX-6 TR: empty-state title (2.6:1)'],
      ['contrast', ALL, 'trash/[trash-empty-state] span', 'UX-6 TR: empty-state hint (2.0:1)'],
      ['contrast', ALL, 'jobs/[jobs-view] span', 'UX-6 JOB: "State" label (4.47:1, borderline)'],
      ['contrast', ALL, 'jobs/[job-row] span', 'UX-6 JOB-3: "n/m tries" (3.6:1)'],
      [
        'contrast',
        ALL,
        'settings-legal/[legal-disclaimer-read] button',
        'UX-7 SET: legal link button (4.2:1)',
      ],
      [
        'contrast',
        ALL,
        'settings-legal/[legal-privacy-read] button',
        'UX-7 SET: legal link button (4.2:1)',
      ],
      // Touch targets (44 px), phones only.
      [
        'target-size',
        PHONES,
        'select-checkbox',
        'UX-4 GAL: the card checkbox is 20 px; it needs a 44 px hit area',
      ],
      ['target-size', PHONES, 'jobs-refresh', 'UX-6 JOB-1: the icon button is 32 px'],
    ] as const
  ).flatMap(([check, viewports, subject, note]) =>
    viewports.map((vp) => [`${check}:${vp}:${subject}`, note] as const),
  ),
);

const MIN_TARGET = 44;
const MIN_CONTRAST = 4.5;

type State = Awaited<ReturnType<BrowserContext['storageState']>>;

// Settles one check's failures against KNOWN: unexpected failures and stale
// KNOWN entries fail, known failures are annotated.
// `lenientStale` for a check whose ratio moves with an animation near its
// threshold (contrast): a known entry that happens not to fail is not stale.
function settle(
  check: string,
  vp: UxViewport,
  failures: Record<string, string>,
  lenientStale = false,
): void {
  const prefix = `${check}:${vp.name}:`;
  const unexpected: string[] = [];
  for (const [subject, detail] of Object.entries(failures)) {
    const note = KNOWN[prefix + subject];
    if (note) {
      test.info().annotations.push({
        type: 'known-failure',
        description: `${prefix}${subject} (${note}): ${detail}`,
      });
      console.log(`KNOWN FAILURE ${prefix}${subject} [${note}]: ${detail}`);
    } else {
      unexpected.push(`${subject}: ${detail}`);
    }
  }
  const stale = lenientStale
    ? []
    : Object.keys(KNOWN).filter(
        (k) => k.startsWith(prefix) && !(k.slice(prefix.length) in failures),
      );
  expect(unexpected, `${check} at ${vp.name}`).toEqual([]);
  expect(stale, `${check} at ${vp.name}: fixed, remove these from KNOWN`).toEqual([]);
}

// The screens of §2 and how to reach each one.
interface Screen {
  id: string;
  narrowOnly?: boolean;
  go(page: Page, vp: UxViewport): Promise<void>;
}

async function gallery(page: Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByTestId('post-card').first()).toBeVisible();
}

const SCREENS: Screen[] = [
  { id: 'gallery', go: gallery },
  {
    id: 'search',
    narrowOnly: true,
    go: async (page) => {
      await gallery(page);
      await page.getByTestId('bottom-nav-search').click();
      await expect(page.locator('[data-testid="gallery-view"] input').first()).toBeFocused();
    },
  },
  {
    id: 'filters',
    go: async (page) => {
      await gallery(page);
      await page.getByTestId('filters-toggle').click();
      await expect(page.getByTestId('filter-drawer')).toBeVisible();
    },
  },
  {
    id: 'selection',
    go: async (page) => {
      await gallery(page);
      await page.getByTestId('select-toggle').click();
      await page.getByTestId('post-card').first().click();
      await expect(page.getByTestId('selection-count')).toBeVisible();
    },
  },
  {
    id: 'post-modal',
    go: async (page) => {
      await gallery(page);
      await openFirstPost(page);
    },
  },
  {
    id: 'drawer',
    narrowOnly: true,
    go: async (page) => {
      await gallery(page);
      await page.getByTestId('sidebar-open').click();
      await expect(page.getByTestId('sidebar')).toBeInViewport();
    },
  },
  {
    id: 'trash',
    go: async (page) => {
      await page.goto('/trash');
      await expect(page.getByTestId('trash-view')).toBeVisible();
    },
  },
  {
    id: 'jobs',
    go: async (page) => {
      await page.goto('/jobs');
      await expect(page.getByTestId('jobs-view')).toBeVisible();
    },
  },
  ...['account', 'language', 'storage', 'legal'].map(
    (section): Screen => ({
      id: `settings-${section}`,
      go: async (page) => {
        await page.goto(`/settings/${section}`);
        await expect(page.getByRole('heading').first()).toBeVisible();
      },
    }),
  ),
  {
    id: 'device',
    go: async (page) => {
      await page.goto('/device');
      await expect(page.getByTestId('device-form')).toBeVisible();
    },
  },
  {
    id: 'not-found',
    go: async (page) => {
      await page.goto('/no-such-page');
      await expect(page.getByRole('heading').first()).toBeVisible();
    },
  },
];

// A tap on a card opens it on desktop; on touch the first tap previews and
// the second opens (P1-02), so tap until the modal is up.
async function openFirstPost(page: Page): Promise<void> {
  const card = page.getByTestId('post-card').first();
  const modal = page.getByTestId('post-modal');
  for (let i = 0; i < 2 && !(await modal.isVisible()); i += 1) {
    await card.click();
    await page.waitForTimeout(150);
  }
  await expect(modal).toBeVisible();
}

// The toolbar of the gallery and its controls (the audit's GAL-1).
const TOOLBAR = [
  'sidebar-open',
  'filters-toggle',
  'sort-toggle',
  'select-toggle',
  'view-mode-toggle',
  'gallery-refresh',
];

// Every test navigates for itself, so one failing check does not stop the rest.

let state: State;
test.beforeAll(async ({ browser }: { browser: Browser }) => {
  state = await signedInState(browser);
});

// The signed-out screens: the sign-in page and the link landing page.
test('captures the signed-out screens at every viewport', async ({ browser }) => {
  for (const vp of UX_VIEWPORTS) {
    const context = await newViewportContext(browser, vp);
    const page = await context.newPage();
    await page.goto('/login');
    await expect(page.getByTestId('login-form')).toBeVisible();
    await shot(page, `ux-sign-in-${vp.name}`);
    if (vp.narrow) {
      const overflow = await overflowReport(page);
      expect(overflow, `sign-in overflow at ${vp.name}`).toEqual({
        pageOverflow: 0,
        scrolledLeft: [],
      });
    }
    await context.close();
  }
});

for (const vp of UX_VIEWPORTS) {
  test.describe(`${vp.name} ${vp.width}x${vp.height}`, () => {
    let context: BrowserContext;
    let page: Page;

    test.beforeAll(async ({ browser }: { browser: Browser }) => {
      context = await newViewportContext(browser, vp, state);
      page = await context.newPage();
    });
    test.afterAll(async () => {
      await context.close();
    });

    for (const screen of SCREENS) {
      if (screen.narrowOnly && !vp.narrow) continue;
      test(`screen: ${screen.id}`, async () => {
        await screen.go(page, vp);
        await shot(page, `ux-${screen.id}-${vp.name}`);
        if (vp.narrow) {
          const overflow = await overflowReport(page);
          const failures: Record<string, string> = {};
          if (overflow.pageOverflow > 0)
            failures.page = `${overflow.pageOverflow}px past the viewport`;
          for (const s of overflow.scrolledLeft)
            failures[s.selector] = `scrollLeft ${s.scrollLeft}`;
          settle(`overflow-${screen.id}`, vp, failures);
        }
      });
    }

    if (vp.narrow) {
      test('no sideways scroll after focusing each toolbar control', async () => {
        await gallery(page);
        const failures: Record<string, string> = {};
        const searchBox = page.locator('[data-testid="gallery-view"] input').first();
        for (const id of [...TOOLBAR, 'search']) {
          const target = id === 'search' ? searchBox : page.getByTestId(id).first();
          if ((await target.count()) === 0) continue;
          await target.focus();
          const overflow = await overflowReport(page);
          if (overflow.pageOverflow > 0 || overflow.scrolledLeft.length > 0) {
            failures[id] =
              `page ${overflow.pageOverflow}px, scrolled ${overflow.scrolledLeft.map((s) => `${s.selector}=${s.scrollLeft}`).join(' ')}`;
            // Put the column back for the next control.
            await page.evaluate(() => {
              for (const el of [
                document.documentElement,
                document.body,
                ...document.querySelectorAll('*'),
              ])
                el.scrollLeft = 0;
            });
          }
        }
        settle('focus-overflow', vp, failures);
      });

      test('controls are in the viewport and at least 44x44', async () => {
        const failures: Record<string, string> = {};
        const measured: string[] = [];
        const measure = async (ids: string[]): Promise<void> => {
          for (const id of ids) {
            const hit = await hitReport(page, id);
            if (!hit) continue;
            measured.push(id);
            const problems: string[] = [];
            if (!hit.inViewport) problems.push('outside the viewport');
            if (hit.width < MIN_TARGET - 0.5 || hit.height < MIN_TARGET - 0.5)
              problems.push(`${hit.width}x${hit.height}`);
            if (problems.length) failures[id] = problems.join(', ');
          }
        };
        await gallery(page);
        await measure([
          ...TOOLBAR,
          'bottom-nav-library',
          'bottom-nav-search',
          'bottom-nav-jobs',
          'bottom-nav-settings',
        ]);
        await page.getByTestId('sidebar-open').click();
        await expect(page.getByTestId('sidebar')).toBeInViewport();
        await page.waitForTimeout(500); // the drawer's slide-in
        await measure(['sidebar-close', 'nav-bookmarks', 'nav-trash', 'nav-jobs', 'nav-settings']);
        await page.getByTestId('sidebar-close').click();
        await page.goto('/');
        await openFirstPost(page);
        await measure([
          'post-modal-close',
          'post-modal-prev',
          'post-modal-next',
          'post-modal-menu',
          'post-modal-more',
        ]);
        await page.goto('/');
        await page.getByTestId('filters-toggle').click();
        await expect(page.getByTestId('filter-drawer')).toBeVisible();
        await page.waitForTimeout(500);
        await measure(['drawer-close', 'drawer-reset', 'drawer-apply']);
        await page.reload();
        await page.getByTestId('select-toggle').click();
        await page.getByTestId('post-card').first().click();
        await measure(['select-checkbox', 'select-cancel']);
        await page.goto('/jobs');
        await expect(page.getByTestId('jobs-view')).toBeVisible();
        await measure(['jobs-refresh']);
        await page.goto('/trash');
        await expect(page.getByTestId('trash-view')).toBeVisible();
        await measure(['trash-select-toggle']);
        console.log(`target-size ${vp.name}: measured ${measured.join(' ')}`);
        expect(measured.length, 'controls found to measure').toBeGreaterThanOrEqual(18);
        settle('target-size', vp, failures);
      });
    }

    test('the post modal is on top of the shell', async () => {
      await gallery(page);
      await openFirstPost(page);
      const failures: Record<string, string> = {};
      const modal = page.getByTestId('post-modal');
      const box = (await modal.boundingBox())!;
      // The header: the close button's centre, then the modal's top band.
      const close = await page.getByTestId('post-modal-close').boundingBox();
      const points: Record<string, [number, number]> = {
        header: close
          ? [close.x + close.width / 2, close.y + close.height / 2]
          : [box.x + box.width / 2, box.y + 8],
        'top-left': [box.x + 24, box.y + 24],
      };
      for (const [name, [x, y]] of Object.entries(points)) {
        const ids = await topmostTestIds(page, x, y);
        if (!ids.includes('post-modal')) failures[name] = `topmost: ${ids.join(' > ') || 'none'}`;
      }
      settle('modal-on-top', vp, failures);
    });

    test('Tab lands on a control with a visible focus ring', async () => {
      await gallery(page);
      await page.mouse.click(1, 1); // start from the page, as a keyboard user does
      await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
      const ring = await tabToFocusRing(page);
      const failures: Record<string, string> = {};
      if (!ring) failures.gallery = 'no visible focus ring after 8 Tab presses';
      settle('focus-visible', vp, failures);
    });

    test('visible text keeps AA contrast', async () => {
      const failures: Record<string, string> = {};
      const scan = async (screen: string, within = 'body'): Promise<void> => {
        for (const [key, ratio] of Object.entries(
          await lowContrastText(page, MIN_CONTRAST, within),
        )) {
          failures[`${screen}/${key}`] = ratio;
        }
      };
      await gallery(page);
      await page.waitForTimeout(600); // entrance animations
      await scan('gallery');
      if (vp.narrow) {
        await page.getByTestId('sidebar-open').click();
        await page.waitForTimeout(500);
        await scan('drawer', '[data-testid="sidebar"]');
        await page.getByTestId('sidebar-close').click();
      }
      await openFirstPost(page);
      await page.waitForTimeout(700);
      await scan('post-modal', '[data-testid="post-modal"]');
      for (const [screen, path] of [
        ['trash', '/trash'],
        ['jobs', '/jobs'],
        ['settings-account', '/settings/account'],
        ['settings-legal', '/settings/legal'],
      ]) {
        await page.goto(path);
        await expect(page.getByRole('heading').first()).toBeVisible();
        await page.waitForTimeout(300);
        await scan(screen);
      }
      settle('contrast', vp, failures, true);
    });
  });
}

// The real server must be the one under test, not a stale origin.
test('the suite runs against its own server', async ({ browser }) => {
  const context = await newContext(browser);
  const res = await context.request.get(`${E2E.apiUrl}/health`);
  expect(res.ok()).toBe(true);
  await context.close();
});
