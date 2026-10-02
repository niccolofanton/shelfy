import React, { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, Cpu, Download, Loader, RefreshCw } from 'lucide-react';
import { useT } from '../i18n';
import { useAnalysis } from '../hooks/useAnalysis';
import type { AiRemoteStatus } from '../../types/electron-api';

// Global banner shown while the configured remote AI node is unreachable. AI
// work is held in the queue meanwhile (see electron/ai-providers.ts aiRoute);
// the user can retry, opt into local models until the app restarts, or download
// the local model first when it is missing.
export default function RemoteAiBanner(): React.ReactElement | null {
  const t = useT('remoteAi');
  const { modelProgress, downloadModel } = useAnalysis();
  const [status, setStatus] = useState<AiRemoteStatus | null>(null);

  const apply = useCallback((next: AiRemoteStatus) => {
    setStatus(next);
    // The analysis model behind the queue may have changed (node back / local
    // chosen): let useAnalysis re-read it.
    window.dispatchEvent(new Event('ai-model-changed'));
  }, []);

  useEffect(() => {
    let alive = true;
    window.electronAPI.getAiRemoteStatus().then((s) => alive && setStatus(s));
    const off = window.electronAPI.onAiRemoteStatus(apply);
    return () => {
      alive = false;
      off();
    };
  }, [apply]);

  // A finished local download flips localReady: re-read so "Use local" appears.
  const downloading = !!modelProgress || !!status?.localDownloading;
  useEffect(() => {
    if (!downloading) window.electronAPI.getAiRemoteStatus().then(setStatus);
  }, [downloading]);

  if (!status?.configured || status.localOverride || status.reachable !== false) return null;

  const pct = Math.round((modelProgress?.progress ?? 0) * 100);
  const secondary =
    'flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40';
  const primary =
    'flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] transition-colors u-press disabled:opacity-40';

  return (
    // Outer layer centers; the inner one animates (the fade keyframes own `transform`).
    <div className="absolute bottom-4 inset-x-0 z-40 flex justify-center px-4 pointer-events-none">
      <div
        role="status"
        className="u-fade-in-up pointer-events-auto flex items-center gap-3 w-full max-w-[760px] px-4 py-2.5 rounded-xl border border-[#2e2e2e] bg-[#1a1a1a] shadow-2xl"
      >
        <AlertTriangle size={16} className="shrink-0" style={{ color: '#f0b429' }} />
        <div className="min-w-0">
          <div className="text-sm font-medium text-gray-100">{t('title')}</div>
          <div className="text-xs text-gray-400">
            {t('body', { name: status.providerName ?? '' })}
          </div>
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          <button
            type="button"
            className={secondary}
            disabled={status.checking}
            onClick={() => window.electronAPI.retryAiRemote().then(apply)}
          >
            {status.checking ? (
              <Loader size={13} className="animate-spin" />
            ) : (
              <RefreshCw size={13} />
            )}
            {status.checking ? t('checking') : t('retry')}
          </button>
          {status.localReady ? (
            <button
              type="button"
              className={primary}
              title={t('useLocalHint')}
              onClick={() => window.electronAPI.useLocalAiModels().then(apply)}
            >
              <Cpu size={13} />
              {t('useLocal')}
            </button>
          ) : (
            <button
              type="button"
              className={primary}
              disabled={downloading}
              onClick={() => void downloadModel()}
            >
              {downloading ? <Loader size={13} className="animate-spin" /> : <Download size={13} />}
              {downloading ? t('downloading', { pct }) : t('downloadLocal')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
