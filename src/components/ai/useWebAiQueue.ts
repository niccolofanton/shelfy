import { useCallback, useEffect, useRef, useState } from 'react';
import type { AiQueueItemState, WebAiQueueApi, WebAiQueuePage } from '../../api/ai/webQueue';

export function useWebAiQueue(api?: WebAiQueueApi, active = true, state?: AiQueueItemState) {
  const [page, setPage] = useState<WebAiQueuePage | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const sequence = useRef(0);
  const refresh = useCallback(async () => {
    if (!api || !active) return;
    const id = ++sequence.current;
    setLoading(true);
    try {
      const value = await api.get({ state });
      if (id === sequence.current) {
        setPage(value);
        setError(null);
      }
    } catch (e) {
      if (id === sequence.current) setError(e);
    } finally {
      if (id === sequence.current) setLoading(false);
    }
  }, [api, active, state]);
  useEffect(() => {
    if (!api || !active) return;
    setPage(null);
    void refresh();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unsubscribe = api.onChanged(() => {
      if (!timer)
        timer = setTimeout(() => {
          timer = undefined;
          void refresh();
        }, 200);
    });
    // A lost live update cannot leave the durable queue stale indefinitely.
    const poll = setInterval(() => {
      void refresh();
    }, 10_000);
    return () => {
      // Invalidate requests still in flight, including refreshes started after mount.
      // eslint-disable-next-line react-hooks/exhaustive-deps
      sequence.current++;
      unsubscribe();
      clearTimeout(timer);
      clearInterval(poll);
    };
  }, [api, active, refresh]);
  const loadMore = async () => {
    if (!api || !page?.cursor || loading) return;
    const id = ++sequence.current;
    setLoading(true);
    try {
      const value = await api.get({ state, cursor: page.cursor });
      if (id === sequence.current) {
        setPage(
          (previous) =>
            previous && {
              ...value,
              items: [
                ...previous.items,
                ...value.items.filter(
                  (item) => !previous.items.some((old) => old.postKey === item.postKey),
                ),
              ],
            },
        );
        setError(null);
      }
    } catch (e) {
      if (id === sequence.current) setError(e);
    } finally {
      if (id === sequence.current) setLoading(false);
    }
  };
  return { page, error, loading, refresh, loadMore };
}

export function useAiQueueStream(api: WebAiQueueApi, active: boolean) {
  const [frames, setFrames] = useState<Record<string, string>>({});
  useEffect(() => {
    if (!active) return;
    return api.onStream((frame) =>
      setFrames((previous) => {
        const entries = Object.entries(previous)
          .filter(([key]) => key !== frame.postKey)
          .slice(-99);
        return { ...Object.fromEntries(entries), [frame.postKey]: frame.text.slice(-8192) };
      }),
    );
  }, [api, active]);
  return frames;
}
