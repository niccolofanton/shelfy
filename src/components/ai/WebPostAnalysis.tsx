import { requestProviderConnection } from './providerConnection';
import React, { useEffect, useMemo, useState } from 'react';
import { useShelfy } from '../../api/ShelfyProvider';
import type { WebAiQueueApi } from '../../api/ai/webQueue';
import type { PostUpdated } from '../postmodal/MetaColumn';
import { useT } from '../../i18n';
import { useFailureText } from '../../hooks/useFailureText';
import { BUTTON, InlineNote } from '../../views/settings/ui';
import { useAiQueueStream, useWebAiQueue } from './useWebAiQueue';
import AnalyzeDialog from './AnalyzeDialog';

export default function WebPostAnalysis({
  api,
  post,
  onPostUpdated,
}: {
  api: WebAiQueueApi;
  post: Shelfy.Post;
  onPostUpdated?: PostUpdated;
}): React.JSX.Element {
  const client = useShelfy();
  const t = useT('aiQueue');
  const failure = useFailureText();
  const { page, error, refresh } = useWebAiQueue(api);
  const frames = useAiQueueStream(api, true);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<unknown>(null);
  const request = useMemo(
    () => ({ selector: { keys: [post.id] }, mode: 'selected' as const }),
    [post.id],
  );
  // Completed analysis fields come from persisted posts, never from stream text.
  useEffect(() => {
    let active = true;
    let sequence = 0;
    const unsubscribe = api.onChanged(() => {
      const id = ++sequence;
      void client
        .getPostsByIds([post.id])
        .then(([value]) => {
          if (active && id === sequence && value)
            onPostUpdated?.(post.id, {
              aiDescription: value.aiDescription,
              aiTags: value.aiTags,
              aiStatus: value.aiStatus,
              aiSaveReason: value.aiSaveReason,
            });
        })
        .catch((e) => {
          if (active) setActionError(e);
        });
    });
    return () => {
      active = false;
      unsubscribe();
    };
  }, [api, client, post.id, onPostUpdated]);
  const item = page?.items.find((value) => value.postKey === post.id);
  const status = item?.status ?? post.aiStatus;
  const queued = status === 'pending' || status === 'analyzing';
  const action = async (operation: () => Promise<unknown>) => {
    setBusy(true);
    setActionError(null);
    try {
      await operation();
      await refresh();
    } catch (e) {
      setActionError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="rounded-lg border border-[#292929] p-3" data-testid="web-post-analysis">
      {!page?.providerState && page && (
        <>
          <InlineNote tone="info">{t('noProvider')}</InlineNote>
          {client.aiProviders?.management && (
            <button className={BUTTON} onClick={() => requestProviderConnection('catalog')}>
              {t('connectProvider')}
            </button>
          )}
        </>
      )}
      {page?.providerState && page.providerState !== 'ok' && (
        <InlineNote tone="info">{t('waitingProvider', { state: page.providerState })}</InlineNote>
      )}
      {status && (
        <p className="text-xs text-gray-400 mb-2">
          {t(`status${status[0].toUpperCase()}${status.slice(1)}`)}
        </p>
      )}
      <div className="flex gap-2">
        {queued ? (
          <button
            className={BUTTON}
            disabled={busy}
            onClick={() => {
              void action(() => api.cancel([post.id]));
            }}
          >
            {t('cancelJob')}
          </button>
        ) : (
          <button
            className={BUTTON}
            data-testid="post-modal-analyze"
            disabled={busy || !page?.providerState}
            onClick={() => setOpen(true)}
          >
            {post.aiDescription ? t('regeneratePost') : t('analyzePost')}
          </button>
        )}
        {status === 'error' && (
          <button
            className={BUTTON}
            disabled={busy}
            onClick={() => {
              void action(() => api.retry([post.id]));
            }}
          >
            {t('retryJob')}
          </button>
        )}
      </div>
      {item?.error && <InlineNote tone="error">{failure({ code: item.error })}</InlineNote>}
      {(error ?? actionError) != null && (
        <InlineNote tone="error">{failure(error ?? actionError)}</InlineNote>
      )}
      {status === 'analyzing' && frames[post.id] && (
        <p className="text-xs text-gray-400 mt-3 whitespace-pre-wrap">{frames[post.id]}</p>
      )}
      {open && (
        <AnalyzeDialog
          api={api}
          request={request}
          onClose={() => setOpen(false)}
          onQueued={() => {
            void refresh();
          }}
        />
      )}
    </section>
  );
}
