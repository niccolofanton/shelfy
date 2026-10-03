import { render } from '@testing-library/react';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { createRef } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';

// Captures the `overscan` react-virtual is actually configured with — the
// definitive signal, since VirtualPostGrid computes its touch-aware default
// once at module load (not a readable prop/DOM attribute). Delegates to the
// real hook so the grid still renders normally.
const seenOverscan: number[] = [];
vi.mock('@tanstack/react-virtual', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tanstack/react-virtual')>();
  return {
    ...actual,
    useVirtualizer: (options: Parameters<typeof useVirtualizer>[0]) => {
      seenOverscan.push(options.overscan ?? -1);
      return actual.useVirtualizer(options);
    },
  };
});

function post(id: string): Shelfy.Post {
  return { id, platform: 'instagram', mediaType: 'image' } as unknown as Shelfy.Post;
}
const POSTS = Array.from({ length: 12 }, (_, i) => post(`p${i}`));

function matchMedia(coarse: boolean) {
  return (query: string) =>
    ({
      matches: query.includes('pointer: coarse') ? coarse : false,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
      onchange: null,
    }) as unknown as MediaQueryList;
}

// Touch-aware overscan default (plan §2.19 "overscan reduced from 6 to 3 rows
// on touch devices") is read from `(pointer: coarse)` once, at module load —
// so each case needs a FRESH module instance with matchMedia mocked first.
async function freshGridWithPointer(coarse: boolean) {
  vi.resetModules();
  vi.stubGlobal('matchMedia', matchMedia(coarse));
  const { default: VirtualPostGrid } = await import('../../src/components/VirtualPostGrid');
  return VirtualPostGrid;
}

describe('VirtualPostGrid overscan default', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    seenOverscan.length = 0;
    vi.resetModules();
  });

  it('defaults to 6 rows on a fine pointer (mouse/trackpad — desktop, unchanged)', async () => {
    const VirtualPostGrid = await freshGridWithPointer(false);
    const scrollRef = createRef<HTMLDivElement>();
    render(<div ref={scrollRef} />);
    render(<VirtualPostGrid posts={POSTS} scrollRef={scrollRef} onOpen={vi.fn()} />);
    expect(seenOverscan.at(-1)).toBe(6);
  });

  it('defaults to 3 rows on a coarse (touch) pointer', async () => {
    const VirtualPostGrid = await freshGridWithPointer(true);
    const scrollRef = createRef<HTMLDivElement>();
    render(<div ref={scrollRef} />);
    render(<VirtualPostGrid posts={POSTS} scrollRef={scrollRef} onOpen={vi.fn()} />);
    expect(seenOverscan.at(-1)).toBe(3);
  });

  it('an explicit overscan prop still wins, on either pointer kind', async () => {
    const VirtualPostGrid = await freshGridWithPointer(true);
    const scrollRef = createRef<HTMLDivElement>();
    render(<div ref={scrollRef} />);
    render(<VirtualPostGrid posts={POSTS} scrollRef={scrollRef} onOpen={vi.fn()} overscan={9} />);
    expect(seenOverscan.at(-1)).toBe(9);
  });
});
