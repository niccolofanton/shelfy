import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';
import type { CreateCollectionOpts, DeleteCollectionResult } from '../../types/electron-api';

export interface UseCollections {
  collections: Shelfy.Collection[];
  error: string | null;
  reload: () => Promise<void>;
  create: (name: string, color: string, opts?: CreateCollectionOpts) => Promise<Shelfy.Collection>;
  remove: (id: number, opts?: { deletePosts?: boolean }) => Promise<DeleteCollectionResult>;
  rename: (id: number, fields: { name?: string; color?: string }) => Promise<void>;
}

// Loads and manages the user's custom sources ("collections"). Kept in App so
// the Sidebar (which lists them) and the Gallery (which assigns posts to them)
// share a single, refreshable source of truth.
export function useCollections(): UseCollections {
  const t = useT('collectionModal');
  const [collections, setCollections] = useState<Shelfy.Collection[]>([]);
  // Distinguishes "load failed" from "user genuinely has no collections" so the
  // Sidebar can surface an error instead of silently rendering an empty list.
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    try {
      const list = await window.electronAPI.getCollections();
      setCollections(list || []);
      setError(null);
    } catch (e) {
      // Keep the previously loaded list rather than clobbering it to [] — a
      // transient IPC/DB failure shouldn't make persisted collections vanish.
      console.error('useCollections: reload failed', e);
      setError((e instanceof Error ? e.message : null) || t('loadError'));
    }
  }, [t]);

  useEffect(() => {
    reload();
  }, [reload]);

  const create = useCallback(
    async (
      name: string,
      color: string,
      opts: CreateCollectionOpts = {},
    ): Promise<Shelfy.Collection> => {
      const created = await window.electronAPI.createCollection(name, color, opts);
      await reload();
      return created;
    },
    [reload],
  );

  const remove = useCallback(
    async (id: number, opts: { deletePosts?: boolean } = {}): Promise<DeleteCollectionResult> => {
      const res = await window.electronAPI.deleteCollection(id, opts);
      await reload();
      return res;
    },
    [reload],
  );

  const rename = useCallback(
    async (id: number, fields: { name?: string; color?: string }): Promise<void> => {
      await window.electronAPI.updateCollection(id, fields);
      await reload();
    },
    [reload],
  );

  return { collections, error, reload, create, remove, rename };
}
