import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { ReactElement } from 'react';
import PostCard from '../../src/components/PostCard';

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
    });

    it('falls back to the typographic card when there is no image but text exists', () => {
      // basePost: mediaType image, no thumbnails, text present.
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
      const card = screen.getByTestId('text-card');
      expect(within(card).getByText('A test post')).toBeInTheDocument();
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
      render(<PostCard post={basePost} onOpen={vi.fn()} />);
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
      const post = { ...basePost, timestamp: '2024-03-15T12:00:00Z' };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      // The formatted date should appear somewhere in the document
      const dateEl = screen.getByText(/mar/i);
      expect(dateEl).toBeInTheDocument();
    });

    it('does not show timestamp when absent', () => {
      renderHovered(<PostCard post={{ ...basePost, timestamp: null }} onOpen={vi.fn()} />);
      // Only the username text should be in the overlay
      const overlay = screen.getByText('@testuser').closest('div');
      expect(overlay).not.toHaveTextContent(/jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec/i);
    });

    it('shows up to 3 AI tags as micro-chips', () => {
      const post = { ...basePost, aiTags: ['design', 'ui', 'web', 'extra'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      const chips = screen.getByTestId('hover-tags');
      expect(within(chips).getByText('design')).toBeInTheDocument();
      expect(within(chips).getByText('ui')).toBeInTheDocument();
      expect(within(chips).getByText('web')).toBeInTheDocument();
      expect(within(chips).queryByText('extra')).toBeNull();
    });

    it('falls back to user tags when there are no AI tags', () => {
      const post = { ...basePost, aiTags: [], userTags: ['mio-tag'] };
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
      renderHovered(<PostCard post={basePost} onOpen={vi.fn()} />);
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
      const post = { ...basePost, aiDescription: 'Una descrizione', aiTags: ['design', 'ui'] };
      renderHovered(<PostCard post={post} onOpen={vi.fn()} />);
      expect(screen.getByTitle(/generati dall'AI/i)).toBeInTheDocument();
    });

    it('lives in the hover overlay, not in the rest-state chips', () => {
      const post = { ...basePost, aiDescription: 'Una descrizione', aiTags: ['design'] };
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
        <PostCard post={{ ...basePost, mediaType: 'image' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-image')).not.toBeNull();
    });

    it('renders the video icon for video mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...basePost, mediaType: 'video' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-video')).not.toBeNull();
    });

    it('renders the layers icon for carousel mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...basePost, mediaType: 'carousel' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-layers')).not.toBeNull();
    });

    it('renders the align-left icon for text mediaType', () => {
      const { container } = render(
        <PostCard post={{ ...basePost, mediaType: 'text' as const }} onOpen={vi.fn()} />,
      );
      expect(container.querySelector('.lucide-align-left')).not.toBeNull();
    });

    it('falls back to the image icon for unknown mediaType', () => {
      const post = { ...basePost, mediaType: 'unknown' } as unknown as Shelfy.Post;
      const { container } = render(<PostCard post={post} onOpen={vi.fn()} />);
      expect(container.querySelector('.lucide-image')).not.toBeNull();
    });

    it('shows the media count for a multi-image post (carousel)', () => {
      render(
        <PostCard
          post={{ ...basePost, mediaType: 'carousel' as const, mediaCount: 4 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.getByText('4')).toBeInTheDocument();
    });

    it('shows the media count for a multi-image tweet (images)', () => {
      render(
        <PostCard
          post={{ ...basePost, mediaType: 'images' as const, mediaCount: 3 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.getByText('3')).toBeInTheDocument();
    });

    it('does not show a count when mediaCount is 1', () => {
      render(
        <PostCard
          post={{ ...basePost, mediaType: 'carousel' as const, mediaCount: 1 }}
          onOpen={vi.fn()}
        />,
      );
      expect(screen.queryByText('1')).toBeNull();
    });
  });
});
