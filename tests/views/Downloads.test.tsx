import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { ComponentProps } from 'react';
import Downloads from '../../src/views/Downloads';

vi.mock('@tanstack/react-virtual', () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () =>
      Array.from({ length: count }, (_, index) => ({ index, start: index * 92 })),
    getTotalSize: () => count * 92,
    measureElement: () => {},
  }),
}));

type DownloadJob = ComponentProps<typeof Downloads>['downloads']['jobs'][number];

const baseJob: DownloadJob = {
  key: 'post-1:thumbnail',
  postId: 'post-1',
  platform: 'twitter',
  assetType: 'thumbnail',
  status: 'done',
  progress: 1,
  authorUsername: 'creator',
};

function setup(jobs: DownloadJob[]) {
  const cancelJob = vi.fn();
  const retryJob = vi.fn();
  render(
    <Downloads
      downloads={{
        jobs,
        stats: { total: 2, thumbnails: 1, images: 0, videos: 0 },
        refresh: vi.fn(),
        clearAll: vi.fn(),
        clearCompleted: vi.fn(),
        cancelJob,
        retryJob,
        isPaused: false,
        pauseAll: vi.fn(),
        resumeAll: vi.fn(),
      }}
    />,
  );
  return { cancelJob, retryJob };
}

describe('Downloads post groups', () => {
  it('counts files separately from posts and shows preview/content steps', () => {
    setup([
      baseJob,
      {
        ...baseJob,
        key: 'post-1:image:0',
        assetType: 'image',
        mediaPosition: 0,
        status: 'pending',
        progress: 0,
      },
      {
        ...baseJob,
        key: 'post-1:video:1',
        assetType: 'video',
        mediaPosition: 1,
        status: 'downloading',
        progress: 0.5,
      },
      {
        ...baseJob,
        key: 'post-2:thumbnail',
        postId: 'post-2',
        authorUsername: 'other',
        status: 'pending',
        progress: 0,
      },
    ]);

    expect(screen.getAllByTestId('download-post-group')).toHaveLength(2);
    expect(screen.queryAllByTestId('download-job')).toHaveLength(0);
    expect(screen.getByText('1 / 4 file scaricati da 2 post')).toBeInTheDocument();

    const creator = screen.getAllByTestId('download-post-group')[0];
    expect(within(creator).getByText('Anteprima 1/1')).toBeInTheDocument();
    expect(within(creator).getByText('Contenuti 0/2')).toBeInTheDocument();
  });

  it('expands one post and preserves per-file cancellation and retry', () => {
    const { cancelJob, retryJob } = setup([
      baseJob,
      {
        ...baseJob,
        key: 'post-1:image:0',
        assetType: 'image',
        mediaPosition: 0,
        status: 'pending',
        progress: 0,
      },
      {
        ...baseJob,
        key: 'post-1:video:1',
        assetType: 'video',
        mediaPosition: 1,
        status: 'error',
        progress: 0,
        error: 'download failed',
      },
    ]);

    const group = screen.getByTestId('download-post-group');
    fireEvent.click(within(group).getByRole('button', { expanded: false }));
    expect(within(group).getAllByTestId('download-job')).toHaveLength(3);

    fireEvent.click(within(group).getByTestId('job-cancel'));
    expect(cancelJob).toHaveBeenCalledWith('post-1:image:0');
    fireEvent.click(within(group).getByTestId('job-retry'));
    expect(retryJob).toHaveBeenCalledWith('post-1:video:1');
  });
});
