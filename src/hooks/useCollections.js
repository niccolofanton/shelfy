import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

// Loads and manages the user's custom sources ("collections"). Kept in App so
// the Sidebar (which lists them) and the Gallery (which assigns posts to them)
// share a single, refreshable source of truth.
export function useCollections() {
  const t = useT('collectionModal');
  const [collections, setCollections] = useState([]);
  // Distinguishes "load failed" from "user genuinely has no collections" so the
  // Sidebar can surface an error instead of silently rendering an empty list.
  const [error, setError] = useState(null);

  const reload = useCallback(async () => {
    try {
      const list = await window.electronAPI.getCollections();
      setCollections(list || []);
      setError(null);
    } catch (e) {
      // Keep the previously loaded list rather than clobbering it to [] — a
      // transient IPC/DB failure shouldn't make persisted collections vanish.
      console.error('useCollections: reload failed', e);
      setError(e?.message || t('loadError'));
    }
  }, [t]);

  useEffect(() => {
    reload();
  }, [reload]);

  const create = useCallback(
    async (name, color, opts = {}) => {
      const created = await window.electronAPI.createCollection(name, color, opts);
      await reload();
      return created;
    },
    [reload],
  );

  const remove = useCallback(
    async (id, opts = {}) => {
      const res = await window.electronAPI.deleteCollection(id, opts);
      await reload();
      return res;
    },
    [reload],
  );

  const rename = useCallback(
    async (id, fields) => {
      await window.electronAPI.updateCollection(id, fields);
      await reload();
    },
    [reload],
  );

  return { collections, error, reload, create, remove, rename };
}
