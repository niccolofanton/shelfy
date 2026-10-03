import { useCallback, useEffect, useRef, useState } from 'react';
import type { LibraryFacets, LibraryFacetsApi } from '../api/facets';

export function useLibraryFacets(api: LibraryFacetsApi | undefined, open: boolean) {
  const [facets, setFacets] = useState<LibraryFacets | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const generation = useRef(0);
  const controller = useRef<AbortController>();
  const refresh = useCallback(async () => {
    if (!api || !open) return;
    controller.current?.abort();
    const signal = new AbortController();
    controller.current = signal;
    const request = ++generation.current;
    setLoading(true);
    try {
      const result = await api.get(signal.signal);
      if (request === generation.current) {
        setFacets(result);
        setError(null);
      }
    } catch (cause) {
      if (request === generation.current && !signal.signal.aborted) setError(cause);
    } finally {
      if (request === generation.current) setLoading(false);
    }
  }, [api, open]);
  useEffect(() => {
    setFacets(null);
    setError(null);
    if (!api || !open) return;
    void refresh();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const off = api.onChanged(() => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        void refresh();
      }, 200);
    });
    return () => {
      // Fence responses that outlive this open drawer or account.
      // eslint-disable-next-line react-hooks/exhaustive-deps
      generation.current++;
      controller.current?.abort();
      clearTimeout(timer);
      off();
    };
  }, [api, open, refresh]);
  return { facets, error, loading, refresh };
}
