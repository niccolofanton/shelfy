import React from 'react';
import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { getElectronClient } from '../../src/api/electronClient';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import type { ActivityNotification, NotificationPage } from '../../src/api/activity';
import type { Job, JobUpdate } from '../../src/api/jobs';
import { useWebActivity } from '../../src/hooks/useWebActivity';
import {
  activityKindLabel,
  notificationLabel,
  notificationTarget,
  registerActivityKind,
} from '../../src/api/activityRegistry';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const notification = (id = 1): ActivityNotification => ({
  id,
  kind: 'job',
  code: 'job.succeeded',
  params: {},
  target: '/jobs',
  createdAt: 100,
  readAt: null,
});
const job = (id = 1): Job => ({
  id,
  kind: 'bulk',
  state: 'running',
  progress: 0.1,
  stage: null,
  postKey: null,
  errorCode: null,
  attempts: 0,
  maxAttempts: 3,
  runAt: 0,
  createdAt: 0,
  updatedAt: 0,
  finishedAt: null,
});
function fixture() {
  const notifications = new Set<(n: ActivityNotification) => void>();
  const refreshes = new Set<() => void>();
  const updates = new Set<(u: JobUpdate) => void>();
  const activity = {
    list: vi.fn(
      async (): Promise<NotificationPage> => ({
        items: [notification()],
        nextCursor: null,
        unreadCount: 1,
      }),
    ),
    read: vi.fn(async () => ({ updated: 1, unreadCount: 0 })),
    onNotification: (listener: (n: ActivityNotification) => void) => {
      notifications.add(listener);
      return () => {
        notifications.delete(listener);
      };
    },
    onRefresh: (listener: () => void) => {
      refreshes.add(listener);
      return () => {
        refreshes.delete(listener);
      };
    },
  };
  const jobs = {
    list: vi.fn(async () => ({ items: [job(2), job(1)], nextCursor: null as string | null })),
    summary: vi.fn(async () => []),
    cancel: vi.fn(async (id: number) => ({ ...job(id), state: 'cancelled' as const })),
    retry: vi.fn(async (id: number) => job(id)),
    pauseQueue: vi.fn(),
    resumeQueue: vi.fn(),
    cancelQueue: vi.fn(),
    clearFinishedQueue: vi.fn(),
    onUpdate: (listener: (u: JobUpdate) => void) => {
      updates.add(listener);
      return () => {
        updates.delete(listener);
      };
    },
  };
  const client = { ...getElectronClient(), activity, jobs };
  function wrapper({ children }: { children: React.ReactNode }) {
    return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
  }
  return {
    activity,
    jobs,
    notifications,
    refreshes,
    updates,
    wrapper,
    emit: (u: JobUpdate) => updates.forEach((listener) => listener(u)),
  };
}

describe('web Activity sources', () => {
  it('retains parallel progress received during the first snapshot and drops a succeeded job', async () => {
    const f = fixture();
    const pending = deferred<{ items: Job[]; nextCursor: null }>();
    f.jobs.list.mockReturnValueOnce(pending.promise);
    const { result, unmount } = renderHook(useWebActivity, { wrapper: f.wrapper });
    act(() => {
      f.emit({ ...job(1), progress: 0.8 });
      f.emit({ ...job(2), progress: 0.6 });
    });
    await act(async () => {
      pending.resolve({ items: [job(2), job(1)], nextCursor: null });
    });
    expect(result.current.jobs.map((j) => j.progress)).toEqual([0.6, 0.8]);
    act(() => f.emit({ ...job(1), state: 'succeeded', progress: 1 }));
    expect(result.current.jobs.map((j) => j.id)).toEqual([2]);
    unmount();
    expect(f.updates.size).toBe(0);
    expect(f.notifications.size).toBe(0);
    expect(f.refreshes.size).toBe(0);
  });
  it('merges notifications arriving during fetch once and reloads persisted reads on reconnect', async () => {
    const f = fixture();
    const pending = deferred<NotificationPage>();
    f.activity.list.mockReturnValueOnce(pending.promise);
    const { result } = renderHook(useWebActivity, { wrapper: f.wrapper });
    act(() => {
      f.notifications.forEach((l) => l(notification(2)));
      f.notifications.forEach((l) => l(notification(2)));
    });
    await act(async () =>
      pending.resolve({ items: [notification()], nextCursor: null, unreadCount: 1 }),
    );
    expect(result.current.notifications.map((n) => n.id)).toEqual([2, 1]);
    expect(result.current.unread).toBe(2);
    f.activity.list.mockResolvedValue({
      items: [
        { ...notification(2), readAt: 300 },
        { ...notification(), readAt: 300 },
      ],
      nextCursor: null,
      unreadCount: 0,
    });
    act(() => f.refreshes.forEach((l) => l()));
    await waitFor(() => expect(result.current.unread).toBe(0));
    act(() => f.notifications.forEach((l) => l(notification(2))));
    expect(result.current.notifications[0].readAt).toBe(300);
    expect(result.current.unread).toBe(0);
  });
  it('marks only the displayed watermark and preserves later notifications', async () => {
    const f = fixture();
    const pending = deferred<{ updated: number; unreadCount: number }>();
    f.activity.read.mockReturnValueOnce(pending.promise);
    const { result } = renderHook(useWebActivity, { wrapper: f.wrapper });
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      void result.current.markAllRead();
    });
    expect(f.activity.read).toHaveBeenCalledWith({ upTo: 1 });
    act(() => f.notifications.forEach((l) => l(notification(2))));
    f.activity.list.mockResolvedValue({
      items: [notification(2), { ...notification(), readAt: 200 }],
      nextCursor: null,
      unreadCount: 1,
    });
    await act(async () => pending.resolve({ updated: 1, unreadCount: 1 }));
    await waitFor(() => expect(result.current.notifications[0].id).toBe(2));
    expect(result.current.notifications[0].readAt).toBeNull();
    expect(result.current.unread).toBe(1);
  });
  it('keeps unread markers when read fails and allows retry without duplicate requests', async () => {
    const f = fixture();
    const pending = deferred<{ updated: number; unreadCount: number }>();
    f.activity.read.mockReturnValueOnce(pending.promise);
    const { result } = renderHook(useWebActivity, { wrapper: f.wrapper });
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      void result.current.markRead(1);
      void result.current.markRead(1);
    });
    expect(f.activity.read).toHaveBeenCalledTimes(1);
    await act(async () => pending.resolve({ updated: 0, unreadCount: 1 }));
    f.activity.read.mockRejectedValueOnce(new Error('offline'));
    await act(async () => result.current.markRead(1));
    expect(result.current.error).toBeInstanceOf(Error);
    expect(result.current.notifications[0].readAt).toBeNull();
  });
  it('uses the API to discover unseen ids and fetches every active page', async () => {
    const f = fixture();
    f.jobs.list
      .mockResolvedValueOnce({ items: [job(2)], nextCursor: 'next' })
      .mockResolvedValueOnce({ items: [job(1)], nextCursor: null });
    const { result } = renderHook(useWebActivity, { wrapper: f.wrapper });
    await waitFor(() => expect(result.current.jobs).toHaveLength(2));
    f.jobs.list.mockResolvedValue({ items: [job(3), job(2), job(1)], nextCursor: null });
    act(() => f.emit({ ...job(3), progress: 0.7 }));
    await waitFor(() => expect(result.current.jobs).toHaveLength(3));
    expect(result.current.jobs[0].progress).toBe(0.7);
  });
});

describe('activity registry', () => {
  it('provides localized labels, safe targets and additive future registrations', () => {
    expect(activityKindLabel('en', 'bulk')).toBe('Bulk action');
    expect(activityKindLabel('it', 'migrate')).toBe('Aggiornamento della libreria');
    expect(activityKindLabel('en', 'new.job_kind')).toBe('New Job Kind');
    registerActivityKind('future.sync', { labelKey: 'activity.syncTitle' });
    expect(activityKindLabel('en', 'future.sync')).toContain('Syncing');
    expect(
      notificationLabel('en', {
        ...notification(),
        code: 'quota.exceeded',
        params: { usedBytes: 50, quotaBytes: 100 },
      }),
    ).toContain('50 of 100');
    expect(notificationTarget('/settings/storage')).toEqual({
      name: 'settings',
      section: 'storage',
    });
    expect(notificationTarget('ig_1')).toEqual({ name: 'post', key: 'ig_1' });
    for (const target of ['//evil.test', 'https://evil.test', '/jobs\\evil', '/login', '/p/%zz'])
      expect(notificationTarget(target)).toBeNull();
  });
});
