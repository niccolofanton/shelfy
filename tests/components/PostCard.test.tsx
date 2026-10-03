import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { ReactElement, ReactNode } from 'react';
import PostCard from '../../src/components/PostCard';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { desktopCapabilities } from '../../src/api/electronClient';
import type { ShelfyClient } from '../../src/api/ShelfyClient';

// The hover overlay (author/timestamp/tags/AI badge/offline icon), the quick-select
// checkbox and the <video> preview mount lazily on first hover/focus — keeping the
// at-rest mount cheap so the virtualizer can reveal rows during a fast scroll
// without jank. Tests asserting on that hover-only chrome render through this
// helper, which fires the hover so the chrome is in the DOM (matching real UX).
function renderHovered(ui: ReactElement) {
  const result = render(ui);
  fireEvent.mouseEnter(screen.getByTestId('post-card'));
  return result;
}

const basePost = {
  postUrl: 'https://example.com/post/1',
  authorUsername: 'testuser',
  platform: 'instagram',
  mediaType: 'image',
  text: 'A test post',
  timestamp: null,
  thumbnailUrl: null,
  thumbnailPath: null,
} as unknown as Shelfy.Post;
const mediaPost = { ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' };

beforeEach(() => {
  global.open = vi.fn();
});

describe('PostCard', () => {
  describe('image rendering', () => {
    it('renders img with thumbnailUrl as src when no thumbnailPath', () => {
      const post = { ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const img = screen.getByRole('img');
      expect(img).toHaveAttribute('src', 'https://cdn.example.com/thumb.jpg');
    });

    it('uses asset:// protocol (downscaled tile variant) when thumbnailPath is set', () => {
      const post = {
        ...basePost,
        thumbnailPath: '/Users/USERNAME/images/thumb.jpg',
        thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const img = screen.getByRole('img');
      // ?w= asks the asset protocol for a cached downscaled copy — grid tiles
      // must never decode the full-resolution original.
      expect(img).toHaveAttribute(
        'src',
        `asset://media/${encodeURIComponent('/Users/USERNAME/images/thumb.jpg')}?w=640`,
      );
    });

    it('uses a local cached preview before an expired remote URL', () => {
      const post = {
        ...basePost,
        previewPath: '/cached/preview.jpg',
        thumbnailUrl: 'https://cdn.example.com/expired.jpg',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.getByRole('img')).toHaveAttribute(
        'src',
        `asset://media/${encodeURIComponent('/cached/preview.jpg')}?w=640`,
      );
    });

    it('requests repair when a remote cover fails to load', () => {
      const post = {
        ...basePost,
        id: 'expired-post',
        thumbnailUrl: 'https://cdn.example.com/expired.jpg',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      fireEvent.error(screen.getByRole('img'));
      expect(window.electronAPI.repairPreview).toHaveBeenCalledWith('expired-post');
      expect(screen.queryByRole('img')).toBeNull();
    });

    it('requests repair when a saved local cover is missing', () => {
      const post = {
        ...basePost,
        id: 'missing-local-cover',
        thumbnailPath: '/old/profile/assets/cover.jpg',
        thumbnailUrl: 'https://cdn.example.com/expired.jpg',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      fireEvent.error(screen.getByRole('img'));
      expect(window.electronAPI.repairPreview).toHaveBeenCalledWith('missing-local-cover');
    });

    it('renders no <img> when neither thumbnailPath nor thumbnailUrl', () => {
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
      expect(screen.queryByRole('img')).toBeNull();
    });
  });

  describe('blur-up placeholder', () => {
    const blurUri = 'data:image/jpeg;base64,AAAA';

    it('paints the blurred placeholder under the still-loading tile', () => {
      const post = {
        ...basePost,
        thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
        thumbBlur: blurUri,
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const blur = screen.getByTestId('blur-placeholder');
      expect(blur).toHaveAttribute('src', blurUri);
      // The real tile starts transparent and fades in over the placeholder.
      expect(screen.getByRole('img')).toHaveClass('opacity-0');
    });

    it('fades the tile in on load and drops the placeholder once settled', () => {
      const post = {
        ...basePost,
        thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
        thumbBlur: blurUri,
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const img = screen.getByRole('img');
      fireEvent.load(img);
      expect(img).toHaveClass('opacity-100');
      // The placeholder stays mounted through the cross-fade…
      expect(screen.getByTestId('blur-placeholder')).toBeInTheDocument();
      // …and unmounts when the opacity transition completes.
      fireEvent.transitionEnd(img);
      expect(screen.queryByTestId('blur-placeholder')).toBeNull();
    });

    it('renders no placeholder when the post has no thumbBlur', () => {
      const post = { ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.queryByTestId('blur-placeholder')).toBeNull();
    });

    it('renders no placeholder for typographic text cards', () => {
      const post = { ...basePost, mediaType: 'text' as const, thumbBlur: blurUri };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.queryByTestId('blur-placeholder')).toBeNull();
    });
  });

  describe('typographic card (text posts)', () => {
    it('renders the post text as a typographic card for text mediaType', () => {
      const post = { ...basePost, mediaType: 'text' as const, text: 'Solo parole, niente media' };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const card = screen.getByTestId('text-card');
      expect(within(card).getByText('Solo parole, niente media')).toBeInTheDocument();
      expect(within(card).getByText('@testuser')).toBeInTheDocument();
      expect(screen.queryByTestId('platform-chip')).toBeNull();
    });

    it('falls back to the typographic card when there is no image but text exists', () => {
      // basePost: mediaType image, no thumbnails, text present.
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
      const card = screen.getByTestId('text-card');
      expect(within(card).getByText('A test post')).toBeInTheDocument();
    });

    it('keeps the excerpt unobstructed on hover', () => {
      renderHovered(<PostCard post={basePost} onOpen={vi.fn()} />);
      expect(screen.getAllByText('@testuser')).toHaveLength(1);
    });

    it('prefers the typographic card over the image for text mediaType', () => {
      const post = {
        ...basePost,
        mediaType: 'text' as const,
        thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.getByTestId('text-card')).toBeInTheDocument();
      expect(screen.queryByRole('img')).toBeNull();
    });
  });

  describe('no-image fallbacks', () => {
    it('shows page title and domain for a web post without screenshot', () => {
      const post = {
        ...basePost,
        platform: 'web' as const,
        mediaType: 'website' as const,
        text: 'Example Site\n\nSome page content',
        webDomain: 'example.com',
        webMeta: { title: 'Example Site' },
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const fallback = screen.getByTestId('web-fallback');
      expect(within(fallback).getByText('Example Site')).toBeInTheDocument();
      expect(within(fallback).getByText('example.com')).toBeInTheDocument();
    });

    it('shows the domain alone for a web post without screenshot nor title', () => {
      const post = {
        ...basePost,
        platform: 'web' as const,
        mediaType: 'website' as const,
        text: null,
        webDomain: 'example.com',
        webMeta: null,
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const fallback = screen.getByTestId('web-fallback');
      expect(within(fallback).getByText('example.com')).toBeInTheDocument();
    });

    it('shows the user note for a manual bookmark without preview', () => {
      const post = {
        ...basePost,
        platform: 'manual' as const,
        mediaType: 'file' as const,
        text: null,
        userNote: 'La mia nota personale',
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const fallback = screen.getByTestId('manual-fallback');
      expect(within(fallback).getByText('La mia nota personale')).toBeInTheDocument();
    });

    it('shows the bookmark label for a manual bookmark without preview nor note', () => {
      const post = {
        ...basePost,
        platform: 'manual' as const,
        mediaType: 'file' as const,
        text: null,
      };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const fallback = screen.getByTestId('manual-fallback');
      expect(within(fallback).getByText('Bookmark')).toBeInTheDocument();
    });

    it('shows platform glyph + handle for a social post with no image and no text', () => {
      const post = { ...basePost, text: null };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      const fallback = screen.getByTestId('social-fallback');
      expect(within(fallback).getByText('@testuser')).toBeInTheDocument();
    });
  });

  describe('rest state', () => {
    it('renders platform and media-type chips', () => {
      render(
        <PostCard
          post={{ ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.getByTestId('platform-chip')).toBeInTheDocument();
      const chip = screen.getByTestId('mediatype-chip');
      // Solid (more opaque) background, NOT backdrop-blur: the frosted chip used
      // `backdrop-filter`, which re-blurred the moving backdrop every frame under
      // the compositor scroll path and pinned native scrolling at ~40fps. See the
      // note on the bottom identity row in PostCard + e2e/perf-gallery.spec.ts.
      expect(chip.className).toContain('bg-black/65');
      expect(chip.className).not.toContain('backdrop-blur');
    });

    it('does not render the always-on bottom gradient anymore', () => {
      const { container } = render(<PostCard post={basePost} onOpen={vi.fn()} />);
      expect(container.querySelector('.h-20')).toBeNull();
    });

    it('has no resting ring when not selected', () => {
      // The hover treatment is now a pure internal media zoom — no resting hairline
      // ring and no accent ring at rest; the card edge is its own fill on the grid.
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
      const card = screen.getByTestId('post-card');
      expect(card.className).not.toContain('ring-2 ring-[#7B5CFF]');
      expect(card.className).not.toContain('ring-1 ring-white/[0.06]');
    });

    it('replaces the hairline ring with the accent ring when selected', () => {
      render(<PostCard post={basePost} onOpen={vi.fn()} selectable selected />);
      const card = screen.getByTestId('post-card');
      expect(card.className).toContain('ring-2 ring-[#7B5CFF]');
      expect(card.className).not.toContain('ring-white/[0.06]');
    });
  });

  describe('hover overlay', () => {
    it('shows @authorUsername in hover overlay', () => {
      const post = { ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.getByText('@testuser')).toBeInTheDocument();
    });

    it('shows formatted timestamp when present', () => {
      const post = { ...mediaPost, timestamp: '2024-03-15T12:00:00Z' };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      // The formatted date should appear somewhere in the document
      const dateEl = screen.getByText(/mar/i);
      expect(dateEl).toBeInTheDocument();
    });

    it('does not show timestamp when absent', () => {
      renderHovered(
        <PostCard
          post={{ ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg', timestamp: null }}
          onOpen={vi.fn()}
        />,
      );
      // Only the username text should be in the overlay
      const overlay = screen.getByText('@testuser').closest('div');
      expect(overlay).not.toHaveTextContent(/jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec/i);
    });

    it('shows up to 3 AI tags as micro-chips', () => {
      const post = { ...mediaPost, aiTags: ['design', 'ui', 'web', 'extra'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      const chips = screen.getByTestId('hover-tags');
      expect(within(chips).getByText('design')).toBeInTheDocument();
      expect(within(chips).getByText('ui')).toBeInTheDocument();
      expect(within(chips).getByText('web')).toBeInTheDocument();
      expect(within(chips).queryByText('extra')).toBeNull();
    });

    it('falls back to user tags when there are no AI tags', () => {
      const post = { ...mediaPost, aiTags: [], userTags: ['mio-tag'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      expect(within(screen.getByTestId('hover-tags')).getByText('mio-tag')).toBeInTheDocument();
    });

    it('shows the first text line when there are no tags at all', () => {
      const post = {
        ...basePost,
        thumbnailUrl: 'https://cdn.example.com/thumb.jpg',
        text: 'Prima riga\nSeconda riga',
      };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.queryByTestId('hover-tags')).toBeNull();
      expect(screen.getByText('Prima riga')).toBeInTheDocument();
    });
  });

  describe('click behavior', () => {
    it('calls onOpen with the post on click', () => {
      const onOpen = vi.fn();
      render(<PostCard post={basePost} onOpen={onOpen} />);
      fireEvent.click(screen.getByTestId('post-card'));
      // onOpen now forwards the click event as a second arg (used for shift-click
      // range-select in the Gallery grid).
      expect(onOpen).toHaveBeenCalledWith(basePost, expect.anything());
    });
  });

  describe('quick-select', () => {
    it('renders the hover checkbox only when onQuickSelect is provided outside select mode', () => {
      const { rerender } = renderHovered(<PostCard post={basePost} onOpen={vi.fn()} />);
      expect(screen.queryByTestId('quick-select-checkbox')).toBeNull();
      rerender(<PostCard post={basePost} onOpen={vi.fn()} onQuickSelect={vi.fn()} />);
      expect(screen.getByTestId('quick-select-checkbox')).toBeInTheDocument();
    });

    it('does not render the quick-select checkbox in select mode', () => {
      render(<PostCard post={basePost} onOpen={vi.fn()} onQuickSelect={vi.fn()} selectable />);
      expect(screen.queryByTestId('quick-select-checkbox')).toBeNull();
      // The regular select-mode checkbox takes its place.
      expect(screen.getByTestId('select-checkbox')).toBeInTheDocument();
    });

    it('calls onQuickSelect on click without opening the post', () => {
      const onOpen = vi.fn();
      const onQuickSelect = vi.fn();
      renderHovered(<PostCard post={basePost} onOpen={onOpen} onQuickSelect={onQuickSelect} />);
      fireEvent.click(screen.getByTestId('quick-select-checkbox'));
      expect(onQuickSelect).toHaveBeenCalledWith(basePost, expect.anything());
      expect(onOpen).not.toHaveBeenCalled();
    });

    it('activates from the keyboard without opening the post', () => {
      const onOpen = vi.fn();
      const onQuickSelect = vi.fn();
      renderHovered(<PostCard post={basePost} onOpen={onOpen} onQuickSelect={onQuickSelect} />);
      fireEvent.keyDown(screen.getByTestId('quick-select-checkbox'), { key: 'Enter' });
      expect(onQuickSelect).toHaveBeenCalledWith(basePost, expect.anything());
      expect(onOpen).not.toHaveBeenCalled();
    });
  });

  describe('OfflineIcon', () => {
    it('shows the link-only icon when no local asset is present', () => {
      renderHovered(<PostCard post={mediaPost} onOpen={vi.fn()} />);
      expect(screen.getByTitle(/solo link/i)).toBeInTheDocument();
    });

    it('does not mark a cached preview as an offline download', () => {
      renderHovered(
        <PostCard post={{ ...basePost, previewPath: '/cached/preview.jpg' }} onOpen={vi.fn()} />,
      );
      expect(screen.getByTitle(/solo link/i)).toBeInTheDocument();
    });

    it('shows the saved-offline icon when thumbnailPath is set', () => {
      renderHovered(
        <PostCard post={{ ...basePost, thumbnailPath: '/path/to/thumb.jpg' }} onOpen={vi.fn()} />,
      );
      expect(screen.getByTitle(/salvato offline/i)).toBeInTheDocument();
    });

    it('shows the saved-offline icon when only videoPath is set', () => {
      renderHovered(
        <PostCard post={{ ...basePost, videoPath: '/path/to/video.mp4' }} onOpen={vi.fn()} />,
      );
      expect(screen.getByTitle(/salvato offline/i)).toBeInTheDocument();
    });
  });

  describe('AI badge', () => {
    it('shows the AI badge when the post has both an AI description and tags', () => {
      const post = { ...mediaPost, aiDescription: 'Una descrizione', aiTags: ['design', 'ui'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.getByTitle(/generati dall'AI/i)).toBeInTheDocument();
    });

    it('lives in the hover overlay, not in the rest-state chips', () => {
      const post = { ...mediaPost, aiDescription: 'Una descrizione', aiTags: ['design'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      const badge = screen.getByTitle(/generati dall'AI/i);
      expect(screen.getByTestId('mediatype-chip')).not.toContainElement(badge);
      expect(screen.getByTestId('platform-chip')).not.toContainElement(badge);
    });

    it('hides the AI badge when only the description is present', () => {
      const post = { ...basePost, aiDescription: 'Una descrizione', aiTags: [] };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.queryByTitle(/generati dall'AI/i)).toBeNull();
    });

    it('hides the AI badge when only tags are present', () => {
      const post = { ...basePost, aiDescription: null, aiTags: ['design'] };
      render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.queryByTitle(/generati dall'AI/i)).toBeNull();
    });

    it('hides the AI badge when there is no AI analysis', () => {
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
      expect(screen.queryByTitle(/generati dall'AI/i)).toBeNull();
    });
  });

  describe('MediaTypeIcon', () => {
    it('renders the image icon for image mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...mediaPost, mediaType: 'image' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-image')).not.toBeNull();
    });

    it('renders the video icon for video mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...mediaPost, mediaType: 'video' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-video')).not.toBeNull();
    });

    it('renders the layers icon for carousel mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...mediaPost, mediaType: 'carousel' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-layers')).not.toBeNull();
    });

    it('labels a text post within its editorial preview', () => {
      render(<PostCard post={{ ...basePost, mediaType: 'text' as const }} onOpen={vi.fn()} />);
      expect(within(screen.getByTestId('text-card')).getByText('TESTO')).toBeInTheDocument();
    });

    it('falls back to the image icon for unknown mediaType', () => {
      const post = { ...mediaPost, mediaType: 'unknown' } as unknown as Shelfy.Post;
      const { container } = render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(container.querySelector('.lucide-image')).not.toBeNull();
    });

    it('shows the media count for a multi-image post (carousel)', () => {
      render(
        <PostCard
          post={{ ...mediaPost, mediaType: 'carousel' as const, mediaCount: 4 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.getByText('4')).toBeInTheDocument();
    });

    it('shows the media count for a multi-image tweet (images)', () => {
      render(
        <PostCard
          post={{ ...mediaPost, mediaType: 'images' as const, mediaCount: 3 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.getByText('3')).toBeInTheDocument();
    });

    it('does not show a count when mediaCount is 1', () => {
      render(
        <PostCard
          post={{ ...mediaPost, mediaType: 'carousel' as const, mediaCount: 1 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.queryByText('1')).toBeNull();
    });
  });

  // Touch behavior (P1-02): a mouse keeps every test above unchanged (hover,
  // click, the quick-select checkbox). onQuickSelect is provided directly
  // here, exactly as Gallery passes it once P1-14 wires the `bulkActions`
  // capability — the web Playwright suite (web/e2e/responsive-shell.spec.ts)
  // can only check what's observable before then (the gesture is recognized
  // and its trailing click is swallowed).
  describe('long-press to select (touch)', () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it('a touch long-press calls onQuickSelect and swallows the trailing click', () => {
      const onOpen = vi.fn();
      const onQuickSelect = vi.fn();
      render(<PostCard post={basePost} onOpen={onOpen} onQuickSelect={onQuickSelect} />);
      const card = screen.getByTestId('post-card');
      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      vi.advanceTimersByTime(600);
      fireEvent.pointerUp(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      fireEvent.click(card);
      expect(onQuickSelect).toHaveBeenCalledWith(basePost, expect.anything());
      expect(onOpen).not.toHaveBeenCalled();
    });

    it('a mouse press never arms the long-press timer', () => {
      const onOpen = vi.fn();
      const onQuickSelect = vi.fn();
      render(<PostCard post={basePost} onOpen={onOpen} onQuickSelect={onQuickSelect} />);
      const card = screen.getByTestId('post-card');
      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'mouse', clientX: 10, clientY: 10 });
      vi.advanceTimersByTime(600);
      fireEvent.pointerUp(card, { pointerId: 1, pointerType: 'mouse', clientX: 10, clientY: 10 });
      fireEvent.click(card);
      expect(onQuickSelect).not.toHaveBeenCalled();
      expect(onOpen).toHaveBeenCalledWith(basePost, expect.anything());
    });

    it('moving past the tolerance before the delay cancels the long-press', () => {
      const onQuickSelect = vi.fn();
      render(<PostCard post={basePost} onOpen={vi.fn()} onQuickSelect={onQuickSelect} />);
      const card = screen.getByTestId('post-card');
      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      fireEvent.pointerMove(card, { pointerId: 1, pointerType: 'touch', clientX: 40, clientY: 10 });
      vi.advanceTimersByTime(600);
      fireEvent.pointerUp(card, { pointerId: 1, pointerType: 'touch', clientX: 40, clientY: 10 });
      expect(onQuickSelect).not.toHaveBeenCalled();
    });

    it('a long-press while already selecting does not call onQuickSelect again', () => {
      const onQuickSelect = vi.fn();
      render(
        <PostCard
          post={basePost}
          onOpen={vi.fn()}
          onQuickSelect={onQuickSelect}
          selectable
          selected={false}
        />,
      );
      const card = screen.getByTestId('post-card');
      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      vi.advanceTimersByTime(600);
      fireEvent.pointerUp(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      expect(onQuickSelect).not.toHaveBeenCalled();
    });
  });

  describe('tap-to-preview instead of hover (touch)', () => {
    it('a first tap previews without opening; a second tap on the same card opens it', () => {
      const onOpen = vi.fn();
      const post = { ...basePost, thumbnailUrl: 'https://cdn.example.com/thumb.jpg' };
      render(<PostCard post={post} onOpen={onOpen} />);
      const card = screen.getByTestId('post-card');

      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      fireEvent.click(card);
      expect(onOpen).not.toHaveBeenCalled();
      expect(screen.getByTestId('post-card-overlay')).toBeInTheDocument();

      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      fireEvent.click(card);
      expect(onOpen).toHaveBeenCalledWith(post, expect.anything());
    });

    it('a mouse click still opens on the first click, as before', () => {
      const onOpen = vi.fn();
      render(<PostCard post={basePost} onOpen={onOpen} />);
      fireEvent.click(screen.getByTestId('post-card'));
      expect(onOpen).toHaveBeenCalledWith(basePost, expect.anything());
    });

    it('select mode skips the preview step: a tap toggles immediately', () => {
      const onOpen = vi.fn();
      render(<PostCard post={basePost} onOpen={onOpen} selectable />);
      const card = screen.getByTestId('post-card');
      fireEvent.pointerDown(card, { pointerId: 1, pointerType: 'touch', clientX: 10, clientY: 10 });
      fireEvent.click(card);
      expect(onOpen).toHaveBeenCalledWith(basePost, expect.anything());
    });
  });

  // Favicon (plan §1.2 #10 "the SPA makes zero third-party requests"): a
  // stored favicon wins everywhere; without one, the desktop (`localFiles`)
  // still falls back to the live per-domain fetch exactly as before — no
  // desktop post has `webFaviconPath` yet, so this is its unchanged behavior —
  // while a web-shaped client (no `localFiles`) shows the neutral glyph
  // instead of ever reaching out to the bookmarked site itself.
  describe('favicon', () => {
    const webishPost = {
      ...basePost,
      platform: 'web' as const,
      mediaType: 'website' as const,
      text: null,
      webDomain: 'example.com',
      webMeta: null,
    };

    function withCapabilities(caps: Partial<ReturnType<typeof desktopCapabilities>>) {
      const client = {
        capabilities: { ...desktopCapabilities('darwin'), ...caps },
        media: {
          file: (ref: string | null | undefined) => ref ?? null,
          tile: (ref: string | null | undefined) => ref ?? null,
          isStored: () => true,
        },
      } as unknown as ShelfyClient;
      function Provider({ children }: { children: ReactNode }) {
        return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
      }
      return Provider;
    }

    // The favicon's `alt=""` is intentional (decorative; the domain text next
    // to it already names the site), which drops its accessible role to
    // "presentation" — queried by tag, not role, for that reason.
    const favicon = (container: HTMLElement) => container.querySelector('img');

    it('desktop (localFiles, no stored favicon) falls back to the live domain fetch, unchanged', () => {
      render(<PostCard post={webishPost} onOpen={vi.fn()} />, {
        wrapper: withCapabilities({ localFiles: true }),
      });
      expect(favicon(screen.getByTestId('web-fallback'))).toHaveAttribute(
        'src',
        'https://example.com/favicon.ico',
      );
    });

    it('a stored favicon wins over the live fetch, on the desktop too', () => {
      const post = { ...webishPost, webFaviconPath: '/media/abc.png' };
      render(<PostCard post={post} onOpen={vi.fn()} />, {
        wrapper: withCapabilities({ localFiles: true }),
      });
      expect(favicon(screen.getByTestId('web-fallback'))).toHaveAttribute('src', '/media/abc.png');
    });

    it('the web client (no localFiles) never hits the domain: neutral glyph without a stored favicon', () => {
      render(<PostCard post={webishPost} onOpen={vi.fn()} />, {
        wrapper: withCapabilities({ localFiles: false }),
      });
      expect(favicon(screen.getByTestId('web-fallback'))).toBeNull();
    });

    it('the web client still shows a stored favicon', () => {
      const post = { ...webishPost, webFaviconPath: '/media/abc.png' };
      render(<PostCard post={post} onOpen={vi.fn()} />, {
        wrapper: withCapabilities({ localFiles: false }),
      });
      expect(favicon(screen.getByTestId('web-fallback'))).toHaveAttribute('src', '/media/abc.png');
    });
  });

  // Viewport-first fetchpriority (plan §2.19): threaded from the grid's own
  // first-paint gate (VirtualPostGrid/InfiniteCanvas), not computed here —
  // PostCard only has to forward whatever it's handed.
  describe('priority', () => {
    it('defaults to auto', () => {
      render(<PostCard post={mediaPost} onOpen={vi.fn()} />);
      expect(screen.getByTestId('card-image')).toHaveAttribute('fetchpriority', 'auto');
    });

    it('is high when the card is marked priority', () => {
      render(<PostCard post={mediaPost} onOpen={vi.fn()} priority />);
      expect(screen.getByTestId('card-image')).toHaveAttribute('fetchpriority', 'high');
    });
  });
});
