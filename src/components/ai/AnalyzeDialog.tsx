import React, { useEffect, useId, useState } from 'react';
import { createPortal } from 'react-dom';
import type { AnalyzeRequest, AnalyzeResult, WebAiQueueApi } from '../../api/ai/webQueue';
import { useDialog } from '../../hooks/useDialog';
import { useFailureText } from '../../hooks/useFailureText';
import { useT } from '../../i18n';
import { BUTTON, PRIMARY_BUTTON, InlineNote, Loading } from '../../views/settings/ui';

export default function AnalyzeDialog({
  api,
  request,
  onClose,
  onQueued,
}: {
  api: WebAiQueueApi;
  request: AnalyzeRequest;
  onClose: () => void;
  onQueued: (count: number) => void;
}): React.JSX.Element {
  const t = useT('aiQueue');
  const failure = useFailureText();
  const title = useId();
  const [result, setResult] = useState<AnalyzeResult | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const ref = useDialog({ onClose, closeOnEscape: !busy });
  useEffect(() => {
    let active = true;
    setResult(null);
    setError(null);
    void api
      .estimate(request)
      .then((value) => {
        if (active) setResult(value);
      })
      .catch((e) => {
        if (active) setError(e);
      });
    return () => {
      active = false;
    };
  }, [api, request, attempt]);
  const confirm = async () => {
    if (!result?.confirmToken || busy) return;
    setBusy(true);
    setError(null);
    try {
      const value = await api.confirm(request, result.confirmToken);
      if (!value.queued) throw new Error(t('notQueued'));
      onQueued(value.enqueued);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return createPortal(
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/65 p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title}
        tabIndex={-1}
        className="w-full max-w-md rounded-xl border border-[#333] bg-[#181818] p-6 text-gray-200"
      >
        <h2 id={title} className="text-lg font-semibold">
          {t('previewTitle')}
        </h2>
        <p className="text-xs text-gray-400 mt-2">{t('previewHelp')}</p>
        {!result && !error && <Loading />}
        {result && (
          <dl className="grid grid-cols-2 gap-3 text-sm mt-5" data-testid="analyze-estimate">
            <dt>{t('analyzable')}</dt>
            <dd>{result.counts.analyzable}</dd>
            <dt>{t('waitingMedia')}</dt>
            <dd>{result.counts.waitingForMedia}</dd>
            <dt>{t('alreadyQueued')}</dt>
            <dd>{result.counts.alreadyQueued}</dd>
            <dt>{t('inputTokens')}</dt>
            <dd>{result.estimate.inputTokens.toLocaleString()}</dd>
            <dt>{t('outputTokens')}</dt>
            <dd>{result.estimate.outputTokens.toLocaleString()}</dd>
            <dt>{t('cost')}</dt>
            <dd>
              {result.estimate.costUsd === null
                ? t('unknown')
                : `$${result.estimate.costUsd.toFixed(4)}`}
            </dd>
            <dt>{t('eta')}</dt>
            <dd>
              {result.estimate.etaMs === null
                ? t('unknown')
                : t('seconds', { count: Math.ceil(result.estimate.etaMs / 1000) })}
            </dd>
          </dl>
        )}
        {result?.counts.waitingForMedia ? (
          <InlineNote tone="info">{t('waitingMediaHelp')}</InlineNote>
        ) : null}
        {error != null && <InlineNote tone="error">{failure(error)}</InlineNote>}
        <div className="mt-6 flex justify-end gap-2">
          <button className={BUTTON} disabled={busy} onClick={onClose}>
            {t('closePreview')}
          </button>
          {!result && error != null && (
            <button className={BUTTON} onClick={() => setAttempt((x) => x + 1)}>
              {t('retryJob')}
            </button>
          )}
          <button
            className={PRIMARY_BUTTON}
            disabled={busy || !result?.confirmToken || !result.counts.analyzable}
            onClick={() => {
              void confirm();
            }}
          >
            {busy ? t('confirming') : t('confirmAnalyze')}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
