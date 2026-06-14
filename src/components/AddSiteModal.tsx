import React, { useEffect, useRef, useState } from 'react';
import { X, Globe, AlertCircle, Loader } from 'lucide-react';
import { useT } from '../i18n';

// Normalise a free-typed URL the way the backend (F1 normalizeInputUrl) does:
// trim and prepend https:// when no scheme is present. Returns null if it still
// doesn't look like a host (no dot).
function normalizeInputUrl(raw: string): string | null {
  const t = (raw || '').trim();
  if (!t) return null;
  const withScheme = /^https?:\/\//i.test(t) ? t : `https://${t}`;
  try {
    const u = new URL(withScheme);
    if (!u.hostname || !u.hostname.includes('.')) return null;
    return u.toString();
  } catch {
    return null;
  }
}

type AddSiteStatus = 'idle' | 'working' | 'error';

interface AddSiteModalProps {
  onClose: () => void;
  onAdded?: (postId?: string) => void;
}

/**
 * AddSiteModal — paste a URL → enqueue a web reference capture.
 *
 * The orchestrator resolves `addWebReference` early: it persists a raw
 * placeholder and queues the enrichment pipeline (capture/extract/analyze) to
 * run in the background, then returns almost immediately. The card appears via
 * the existing `interceptor:newPosts` event the gallery is already subscribed
 * to, and the parent (`onAdded`) closes this modal and opens the Websites tab,
 * where the per-card progress is surfaced. This modal is therefore fire-and-
 * forget: it only shows a brief "queuing" state, not the full pipeline phases.
 *
 * Props:
 *   onClose()        — dismiss the modal.
 *   onAdded(postId?) — fired once the reference is queued (gallery safety reload).
 */
export default function AddSiteModal({ onClose, onAdded }: AddSiteModalProps) {
  const t = useT('addSite');
  const tc = useT('common');
  const [status, setStatus] = useState<AddSiteStatus>('idle');
  const [url, setUrl] = useState<string>('');
  const [maxPages, setMaxPages] = useState<string>('');
  // Single-page mode: capture exactly the pasted URL (an article/guide) without
  // crawling the sitemap. Hides the max-pages hint, which no longer applies.
  const [singlePage, setSinglePage] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Esc closes (unless a capture is mid-flight — mirror ImportModal's guard).
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape' && status !== 'working') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose, status]);

  const normalized = normalizeInputUrl(url);
  const canSubmit = !!normalized && status !== 'working';

  const handleSubmit = async (): Promise<void> => {
    if (!canSubmit || !normalized) return;
    setStatus('working');
    setError(null);

    try {
      const n = parseInt(maxPages, 10);
      const res = await window.electronAPI.addWebReference(
        normalized,
        singlePage ? 1 : Number.isFinite(n) && n > 0 ? n : undefined,
        undefined,
        singlePage,
      );
      // The reference is queued/persisted: the gallery will pick it up via
      // interceptor:newPosts and enrich it on its own. Hand off to the parent,
      // which closes this modal and opens the Websites tab; no success screen.
      const id = (res as { id?: string } | null | undefined)?.id;
      onAdded?.(id);
    } catch (err) {
      console.error('[AddSiteModal] addWebReference error:', err);
      setError(err instanceof Error ? err.message : t('errorFallback'));
      setStatus('error');
    }
  };

  const handleOverlayClick = (): void => {
    if (status === 'working') return;
    onClose();
  };

  const retry = (): void => {
    setStatus('idle');
    setError(null);
  };

  return (
    <div
      data-testid="add-site-modal"
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-6 u-backdrop-in"
      onClick={handleOverlayClick}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-sm overflow-hidden u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 h-12 border-b border-[#2e2e2e]">
          <span className="flex items-center gap-2 text-white text-sm font-semibold font-display">
            <Globe size={16} className="text-[#7B5CFF]" />
            {t('title')}
          </span>
          <button
            onClick={onClose}
            disabled={status === 'working'}
            title={tc('close')}
            className="flex items-center justify-center w-8 h-8 -mr-2 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40"
          >
            <X size={16} />
          </button>
        </div>

        <div key={status} className="u-swap-in">
          {status === 'idle' && (
            <div className="px-5 py-5 space-y-4">
              <p className="text-[#888] text-sm leading-relaxed">{t('intro')}</p>

              <div>
                <label className="block text-xs font-medium text-gray-400 mb-1.5">
                  {t('addressLabel')}
                </label>
                <input
                  ref={inputRef}
                  data-testid="add-site-url-input"
                  type="text"
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') handleSubmit();
                  }}
                  placeholder={t('addressPlaceholder')}
                  className="w-full bg-[#0f0f0f] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] transition-colors"
                />
              </div>

              {/* Single-page toggle: capture only the pasted URL (article/guide),
                  no sitemap crawl. When on, the max-pages hint no longer applies. */}
              <label
                data-testid="add-site-singlepage-toggle"
                className="flex items-start gap-2.5 cursor-pointer select-none"
              >
                <input
                  type="checkbox"
                  checked={singlePage}
                  onChange={(e) => setSinglePage(e.target.checked)}
                  className="mt-0.5 h-4 w-4 shrink-0 rounded border-[#2e2e2e] bg-[#0f0f0f] accent-[#7B5CFF]"
                />
                <span className="flex flex-col">
                  <span className="text-sm text-gray-200">{t('singlePageLabel')}</span>
                  <span className="text-xs text-gray-500">{t('singlePageHint')}</span>
                </span>
              </label>

              {!singlePage && (
                <div className="u-fade-in">
                  <label className="block text-xs font-medium text-gray-400 mb-1.5">
                    {t('maxPagesLabel')}{' '}
                    <span className="text-gray-600">{t('maxPagesOptional')}</span>
                  </label>
                  <input
                    data-testid="add-site-maxpages-input"
                    type="number"
                    min="1"
                    max="8"
                    value={maxPages}
                    onChange={(e) => setMaxPages(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleSubmit();
                    }}
                    placeholder={t('maxPagesPlaceholder')}
                    className="w-full bg-[#0f0f0f] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] transition-colors"
                  />
                </div>
              )}
            </div>
          )}

          {status === 'working' && (
            <div className="px-5 py-6 flex items-center gap-2 text-sm text-[#ccc]">
              <Loader size={16} className="text-[#7B5CFF] animate-spin" />
              <span className="u-fade-in">{t('working')}</span>
            </div>
          )}

          {status === 'error' && (
            <div className="px-5 py-5">
              <div className="flex flex-col items-center gap-3 py-3 mb-4 u-shake">
                <AlertCircle size={36} className="text-red-500 u-pop-in" />
                <p className="text-[#ccc] text-sm text-center">{error}</p>
              </div>
              <div className="flex flex-col gap-2">
                <button
                  onClick={retry}
                  className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors u-press"
                >
                  {tc('retry')}
                </button>
                <button
                  onClick={onClose}
                  className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                >
                  {tc('cancel')}
                </button>
              </div>
            </div>
          )}
        </div>

        {status === 'idle' && (
          <div className="flex justify-end gap-2 px-5 py-3 border-t border-[#2e2e2e]">
            <button
              onClick={onClose}
              className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press"
            >
              {tc('cancel')}
            </button>
            <button
              data-testid="add-site-submit"
              onClick={handleSubmit}
              disabled={!canSubmit}
              className="px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-40 disabled:cursor-not-allowed transition-[background-color,opacity] duration-200 u-press"
            >
              {tc('add')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
