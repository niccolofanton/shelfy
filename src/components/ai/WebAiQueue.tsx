import { requestProviderConnection } from './providerConnection';
import React, { useState } from 'react';
import type { AiQueueItemState, AnalyzeRequest, WebAiQueueApi } from '../../api/ai/webQueue';
import { useT } from '../../i18n';
import { useFailureText } from '../../hooks/useFailureText';
import { BUTTON, PRIMARY_BUTTON, InlineNote, Loading } from '../../views/settings/ui';
import AnalyzeDialog from './AnalyzeDialog';
import { useAiQueueStream, useWebAiQueue } from './useWebAiQueue';

export default function WebAiQueue({
  api,
  active,
  canConnect = false,
  onOpenPost,
}: {
  api: WebAiQueueApi;
  active: boolean;
  canConnect?: boolean;
  onOpenPost?: (key: string) => void;
}): React.JSX.Element {
  const t = useT('aiQueue');
  const failure = useFailureText();
  const [state, setState] = useState<AiQueueItemState>();
  const { page, error, loading, refresh, loadMore } = useWebAiQueue(api, active, state);
  const frames = useAiQueueStream(api, active);
  const [request, setRequest] = useState<AnalyzeRequest | null>(null);
  const [actionError, setActionError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setActionError(null);
    try {
      await action();
      await refresh();
    } catch (e) {
      setActionError(e);
    } finally {
      setBusy(false);
    }
  };
  const pending = (page?.counts.pending ?? 0) + (page?.counts.analyzing ?? 0);
  return (
    <main className="h-full overflow-y-auto px-6 py-5" data-testid="web-ai-queue">
      <h1 className="text-xl text-white font-semibold">{t('heading')}</h1>
      <div className="flex flex-wrap gap-2 mt-4">
        <button
          className={PRIMARY_BUTTON}
          disabled={busy || !page?.providerState}
          onClick={() => setRequest({ selector: { filter: {} }, mode: 'missing' })}
        >
          {t('analyzeMissing')}
        </button>
        <button
          className={BUTTON}
          disabled={busy || !page?.providerState}
          onClick={() => setRequest({ selector: { filter: {} }, mode: 'all' })}
        >
          {t('analyzeAll')}
        </button>
        <button
          className={BUTTON}
          disabled={busy || !page}
          onClick={() => {
            void run(() => (page?.paused ? api.resume() : api.pause()));
          }}
        >
          {page?.paused ? t('resume') : t('pause')}
        </button>
        <button
          className={BUTTON}
          disabled={busy || !pending}
          onClick={() => {
            void run(() => api.cancel());
          }}
        >
          {t('cancelAll')}
        </button>
        <button
          className={BUTTON}
          disabled={busy || !page?.counts.error}
          onClick={() => {
            void run(() => api.retry());
          }}
        >
          {t('retryErrors')}
        </button>
      </div>
      {error != null && <InlineNote tone="error">{failure(error)}</InlineNote>}
      {actionError != null && <InlineNote tone="error">{failure(actionError)}</InlineNote>}
      {page && (
        <>
          {!page.providerState && (
            <>
              <InlineNote tone="info">{t('noProvider')}</InlineNote>
              {canConnect && (
                <button className={BUTTON} onClick={() => requestProviderConnection('catalog')}>
                  {t('connectProvider')}
                </button>
              )}
            </>
          )}
          {page.providerState && page.providerState !== 'ok' && (
            <InlineNote tone="info">
              {t('waitingProvider', { state: page.providerState })}
            </InlineNote>
          )}
          {page.paused && <InlineNote tone="info">{t('paused')}</InlineNote>}
          <div
            className="flex flex-wrap gap-4 text-sm text-gray-300 my-5"
            data-testid="queue-counts"
          >
            <span>
              {t('missing')}: {page.counts.unanalyzed}
            </span>
            <span>
              {t('inQueue')}: {page.counts.pending}
            </span>
            <span>
              {t('statusAnalyzing')}: {page.counts.analyzing}
            </span>
            <span>
              {t('completed')}: {page.counts.done}
            </span>
            <span>
              {t('errors')}: {page.counts.error}
            </span>
            <span>
              {t('eta')}:{' '}
              {page.etaMs === null
                ? t('unknown')
                : t('seconds', { count: Math.ceil(page.etaMs / 1000) })}
            </span>
          </div>
          <div className="flex flex-wrap gap-2 mb-4" aria-label={t('filterState')}>
            {([undefined, 'pending', 'analyzing', 'done', 'error'] as const).map((value) => (
              <button
                key={value ?? 'all'}
                className={state === value ? PRIMARY_BUTTON : BUTTON}
                aria-pressed={state === value}
                onClick={() => setState(value)}
              >
                {value ? t(`status${value[0].toUpperCase()}${value.slice(1)}`) : t('allStates')}
              </button>
            ))}
          </div>
          <ul className="divide-y divide-[#292929]">
            {page.items.map((item) => (
              <li
                key={item.postKey}
                className="py-3 text-sm text-gray-300"
                data-testid={`ai-queue-${item.postKey}`}
              >
                <div className="flex items-center gap-3">
                  <button
                    className="text-white truncate hover:underline text-left flex-1"
                    onClick={() => onOpenPost?.(item.postKey)}
                  >
                    {item.postKey}
                  </button>
                  <span>{t(`status${item.status[0].toUpperCase()}${item.status.slice(1)}`)}</span>
                  {['pending', 'analyzing'].includes(item.status) && (
                    <button
                      className={BUTTON}
                      disabled={busy}
                      onClick={() => {
                        void run(() => api.cancel([item.postKey]));
                      }}
                    >
                      {t('cancelJob')}
                    </button>
                  )}
                  {item.status === 'error' && (
                    <button
                      className={BUTTON}
                      disabled={busy}
                      onClick={() => {
                        void run(() => api.retry([item.postKey]));
                      }}
                    >
                      {t('retryJob')}
                    </button>
                  )}
                </div>
                {item.error && <p className="text-red-400 mt-2">{failure({ code: item.error })}</p>}
                {item.nextAt != null && (
                  <p className="text-gray-500 text-xs mt-2">
                    {t('nextAttempt', { time: new Date(item.nextAt).toLocaleTimeString() })}
                  </p>
                )}
                {item.status === 'analyzing' && frames[item.postKey] && (
                  <p className="text-xs text-gray-400 mt-2 whitespace-pre-wrap" aria-live="polite">
                    {frames[item.postKey]}
                  </p>
                )}
              </li>
            ))}
          </ul>
          {!page.items.length && !loading && (
            <p className="text-sm text-gray-500 my-6">{t('webEmpty')}</p>
          )}
          {page.cursor && (
            <button
              className={BUTTON}
              disabled={loading}
              onClick={() => {
                void loadMore();
              }}
            >
              {t('loadMore')}
            </button>
          )}
        </>
      )}
      {loading && <Loading />}
      {request && (
        <AnalyzeDialog
          api={api}
          request={request}
          onClose={() => setRequest(null)}
          onQueued={() => {
            void refresh();
          }}
        />
      )}
    </main>
  );
}
