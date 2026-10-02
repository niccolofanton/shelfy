import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';
import { useShelfy } from '../api/ShelfyProvider';
import type { CollectionDeleteResult } from '../api/ShelfyClient';

export interface UseCollections {
  collections: Shelfy.Collection[];
  error: string | null;
  reload: () => Promise<void>;
  create: (name: string, color: string) => Promise<Shelfy.Collection>;
  remove: (id: number, opts?: { deletePosts?: boolean }) => Promise<CollectionDeleteResult>;
  rename: (id: number, fields: { name?: string; color?: string }) => Promise<void>;
}

// Loads and manages the user's custom sources ("collections"). Kept in App so
// the Sidebar (which lists them) and the Gallery (which assigns posts to them)
// share a single, refreshable source of truth. Every operation goes through
// the ShelfyClient seam (P1-06), so it works on both clients; `create` dropped
// the platform-linking options no caller ever passed (`CreateCollectionOpts`:
// that auto-link flow writes through its own IPC call directly).
export function useCollections(): UseCollections {
  const t = useT('collectionModal');
  const client = useShelfy();
  const [collections, setCollections] = useState<Shelfy.Collection[]>([]);
  // Distinguishes "load failed" from "user genuinely has no collections" so the
  // Sidebar can surface an error instead of silently rendering an empty list.
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    try {
      const list = await client.listCollections();
      setCollections(list || []);
      setError(null);
    } catch (e) {
      // Keep the previously loaded list rather than clobbering it to [] — a
      // transient IPC/DB failure shouldn't make persisted collections vanish.
      console.error('useCollections: reload failed', e);
      setError((e instanceof Error ? e.message : null) || t('loadError'));
    }
  }, [t, client]);

  useEffect(() => {
    reload();
  }, [reload]);

  const create = useCallback(
    async (name: string, color: string): Promise<Shelfy.Collection> => {
      const created = await client.createCollection(name, color);
      await reload();
      return created;
    },
    [client, reload],
  );

  const remove = useCallback(
    async (id: number, opts: { deletePosts?: boolean } = {}): Promise<CollectionDeleteResult> => {
      const res = await client.deleteCollection(id, opts);
      await reload();
      return res;
    },
    [client, reload],
  );

  const rename = useCallback(
    async (id: number, fields: { name?: string; color?: string }): Promise<void> => {
      await client.updateCollection(id, fields);
      await reload();
    },
    [client, reload],
  );

  return { collections, error, reload, create, remove, rename };
}
