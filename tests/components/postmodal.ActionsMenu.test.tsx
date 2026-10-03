import { render, screen, fireEvent, act, waitFor } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import type { ComponentProps } from 'react';
import ActionsMenu from '../../src/components/postmodal/ActionsMenu';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { createElectronClient } from '../../src/api/electronClient';
import type { ShelfyClient } from '../../src/api/ShelfyClient';

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

describe('postmodal/ActionsMenu — delete (P1-14 bulk seam)', () => {
  it('on the desktop: a two-step confirm calls bulkAction, which still runs the permanent deletePosts', async () => {
    vi.mocked(window.electronAPI.deletePosts).mockResolvedValue({
      ok: true,
      deleted: 1,
      errors: [],
    });
    const { props } = renderMenu();
    fireEvent.click(screen.getByTestId('post-modal-more'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    expect(window.electronAPI.deletePosts).not.toHaveBeenCalled(); // first click only arms it
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    await waitFor(() => expect(window.electronAPI.deletePosts).toHaveBeenCalledWith(['p1']));
    // The desktop never soft-deletes: no undo handle.
    await waitFor(() => expect(props.onPostDeleted).toHaveBeenCalledWith('p1', null));
    expect(props.onClose).toHaveBeenCalled();
  });

  it('on the web: deletes through the seam and surfaces the undo handle', async () => {
    const client: ShelfyClient = {
      ...createElectronClient(),
      bulkAction: vi.fn().mockResolvedValue({ changed: 1, selected: 1, deletedAt: 42, job: null }),
    };
    const onPostDeleted = vi.fn();
    render(
      <ShelfyProvider client={client}>
        <ActionsMenu
          post={basePost}
          url="https://example.com/post/1"
          primaryLocalPath={null}
          isManual={false}
          onPostDeleted={onPostDeleted}
          onClose={vi.fn()}
        />
      </ShelfyProvider>,
    );
    fireEvent.click(screen.getByTestId('post-modal-more'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    await waitFor(() => expect(client.bulkAction).toHaveBeenCalledWith({ keys: ['p1'] }, 'delete'));
    await waitFor(() => expect(onPostDeleted).toHaveBeenCalledWith('p1', 42));
  });

  it('surfaces a failure and leaves the modal open to retry', async () => {
    vi.mocked(window.electronAPI.deletePosts).mockRejectedValue(new Error('db error'));
    const { props } = renderMenu();
    fireEvent.click(screen.getByTestId('post-modal-more'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    fireEvent.click(screen.getByTestId('post-modal-delete-post'));
    expect(await screen.findByTestId('action-error')).toBeInTheDocument();
    expect(props.onPostDeleted).not.toHaveBeenCalled();
    expect(props.onClose).not.toHaveBeenCalled();
  });
});
