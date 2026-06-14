import { render, screen, fireEvent, act, waitFor } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import type { ComponentProps } from 'react';
import ActionsMenu from '../../src/components/postmodal/ActionsMenu';

// A post with no local assets so the "Scarica in locale" entry is rendered.
const basePost = {
  id: 'p1',
  platform: 'instagram',
  thumbnailPath: null,
  imagePath: null,
  videoPath: null,
} as unknown as Shelfy.Post;

type ActionsMenuProps = ComponentProps<typeof ActionsMenu>;

function renderMenu(overrides: Partial<ActionsMenuProps> = {}) {
  const props: ActionsMenuProps = {
    post: basePost,
    url: 'https://example.com/post/1',
    primaryLocalPath: null,
    isManual: false,
    onLocalFilesDeleted: vi.fn(),
    onPostDeleted: vi.fn(),
    onPostUpdated: vi.fn(),
    onClose: vi.fn(),
    ...overrides,
  };
  const utils = render(<ActionsMenu {...props} />);
  return { ...utils, props };
}

// The IPC progress handler captured by the mocked subscription.
const progressHandler = (): ((data: unknown) => void) =>
  vi.mocked(window.electronAPI.onDownloadProgress).mock.calls[0][0];

// Queue a download through the UI so the handler's "downloadQueued" gate opens.
async function queueDownload() {
  fireEvent.click(screen.getByTestId('post-modal-more'));
  fireEvent.click(screen.getByTestId('post-modal-download'));
  // downloadPost resolves {} (no queued count) — the entry flips to "In coda…".
  await waitFor(() => expect(window.electronAPI.downloadPost).toHaveBeenCalledWith('p1'));
}

describe('postmodal/ActionsMenu — onDownloadProgress subscription', () => {
  it('subscribes once per mount and does NOT re-subscribe when the post changes', () => {
    const { rerender, props } = renderMenu();
    expect(window.electronAPI.onDownloadProgress).toHaveBeenCalledTimes(1);

    // Switching to a different post must reuse the same subscription (the old
    // code re-subscribed on every activePost change, duplicating listeners).
    rerender(<ActionsMenu {...props} post={{ ...basePost, id: 'p2' }} />);
    rerender(<ActionsMenu {...props} post={{ ...basePost, id: 'p3' }} />);
    expect(window.electronAPI.onDownloadProgress).toHaveBeenCalledTimes(1);
  });

  it('unsubscribes on unmount', () => {
    const unsub = vi.fn();
    vi.mocked(window.electronAPI.onDownloadProgress).mockReturnValue(unsub);
    const { unmount } = renderMenu();
    expect(unsub).not.toHaveBeenCalled();
    unmount();
    expect(unsub).toHaveBeenCalledTimes(1);
  });

  it('refreshes on a terminal event for the current post even without a queued download (slow-tail asset)', () => {
    const { props } = renderMenu();
    act(() => {
      progressHandler()({ postId: 'p1', status: 'done' });
    });
    // A slow-tail asset (e.g. a video) can land after the settle timer already
    // cleared the "In coda…" spinner. Gating the refresh on the queued flag would
    // silently drop it, leaving the modal on the remote URL and the menu without
    // the "local" actions — so the refresh must NOT be gated on downloadQueued.
    expect(window.electronAPI.getPostsByIds).toHaveBeenCalled();
    expect(props.onLocalFilesDeleted).toHaveBeenCalledWith('p1');
  });

  it("filters by post id inside the handler: another post's events are ignored", async () => {
    const { props } = renderMenu();
    await queueDownload();

    act(() => {
      progressHandler()({ postId: 'other-post', status: 'done' });
    });
    expect(window.electronAPI.getPostsByIds).not.toHaveBeenCalled();
    expect(props.onLocalFilesDeleted).not.toHaveBeenCalled();
  });

  it("refreshes the post on this post's 'done' events", async () => {
    const { props } = renderMenu();
    await queueDownload();

    act(() => {
      progressHandler()({ postId: 'p1', status: 'done' });
    });
    await waitFor(() => expect(window.electronAPI.getPostsByIds).toHaveBeenCalledWith(['p1']));
    expect(props.onLocalFilesDeleted).toHaveBeenCalledWith('p1');
  });

  it('the handler tracks the current post after navigation (stale closure guard)', async () => {
    const { rerender, props } = renderMenu();
    rerender(<ActionsMenu {...props} post={{ ...basePost, id: 'p2' }} />);
    fireEvent.click(screen.getByTestId('post-modal-more'));
    fireEvent.click(screen.getByTestId('post-modal-download'));
    await waitFor(() => expect(window.electronAPI.downloadPost).toHaveBeenCalledWith('p2'));

    // An event for the OLD post must be ignored; one for the new post refreshes it.
    act(() => {
      progressHandler()({ postId: 'p1', status: 'done' });
    });
    expect(window.electronAPI.getPostsByIds).not.toHaveBeenCalled();
    act(() => {
      progressHandler()({ postId: 'p2', status: 'done' });
    });
    await waitFor(() => expect(window.electronAPI.getPostsByIds).toHaveBeenCalledWith(['p2']));
  });
});
