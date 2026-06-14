import React from 'react';
import { HardDriveDownload } from 'lucide-react';
import { useT, useLang, localeTag } from '../../i18n';
import { formatTimestamp, MEDIA_TYPE_KEY } from './helpers';
import WebMetaPanel from './WebMetaPanel';
import AiPanel from './AiPanel';

// RIGHT column of the modal: all the written content — caption, web metadata
// (web references only), AI categorization and the quiet facts/local-files
// byline at the bottom.
export default function MetaColumn({
  post,
  isWeb,
  slideCount,
  hasMultiple,
  onApplyAiFilter,
  onPostUpdated,
  onOpenInWebsites,
  onReanalyzeWeb,
  onAiEditingChange,
}) {
  const t = useT('postModal');
  const { lang } = useLang();

  // Secondary facts, rendered as one quiet byline so they recede behind the caption.
  const dateStr = formatTimestamp(post.timestamp, localeTag(lang));
  const facts = [
    MEDIA_TYPE_KEY[post.mediaType] ? t(MEDIA_TYPE_KEY[post.mediaType]) : post.mediaType,
    hasMultiple ? `${slideCount} ${isWeb ? t('pages') : t('items')}` : null,
    dateStr,
  ].filter(Boolean);

  const localAssets = [
    [post.thumbnailPath, t('assetThumbnail')],
    [post.imagePath, t('assetImage')],
    [post.videoPath, t('assetVideo')],
  ].filter(([p]) => p);

  if (isWeb) {
    /* ── Web reference: caption / metadata / AI / facts ─────────────────────── */
    return (
      <div
        data-testid="post-modal-meta"
        className="w-[380px] shrink-0 overflow-y-auto border-l border-[#2e2e2e] px-4 py-3.5 space-y-3 bg-[#161616] scrollbar-thin scrollbar-thumb-[#2e2e2e]"
      >
        {post.text && (
          <p className="u-fade-in text-[#ececec] text-[14px] leading-relaxed whitespace-pre-wrap break-words">
            {post.text}
          </p>
        )}

        <WebMetaPanel
          post={post}
          onOpenInWebsites={onOpenInWebsites}
          onReanalyzeWeb={onReanalyzeWeb}
        />

        <AiPanel
          post={post}
          onApplyAiFilter={onApplyAiFilter}
          onPostUpdated={onPostUpdated}
          onEditingChange={onAiEditingChange}
        />

        {(facts.length > 0 || localAssets.length > 0) && (
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-[#808080]">
            {facts.map((fact, i) => (
              <React.Fragment key={i}>
                {i > 0 && <span className="text-[#454545]">·</span>}
                <span>{fact}</span>
              </React.Fragment>
            ))}
          </div>
        )}
      </div>
    );
  }

  /* ── Social post: caption / AI categorization / facts + local files ───────── */
  return (
    <div
      data-testid="post-modal-meta"
      className="w-[380px] shrink-0 overflow-y-auto border-l border-[#2e2e2e] px-4 py-3.5 space-y-3 bg-[#161616] scrollbar-thin scrollbar-thumb-[#2e2e2e]"
    >
      {/* Caption — the saved content itself, the hero of this panel */}
      {post.text && (
        <p className="u-fade-in text-[#ececec] text-[15px] leading-relaxed whitespace-pre-wrap break-words">
          {post.text}
        </p>
      )}

      {/* Local VLM categorization (videos only) */}
      <AiPanel
        post={post}
        onApplyAiFilter={onApplyAiFilter}
        onPostUpdated={onPostUpdated}
        onEditingChange={onAiEditingChange}
      />

      {/* Footer — quiet byline of facts, plus links to any local files */}
      {(facts.length > 0 || localAssets.length > 0) && (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-[#808080]">
          {facts.map((fact, i) => (
            <React.Fragment key={i}>
              {i > 0 && <span className="text-[#454545]">·</span>}
              <span>{fact}</span>
            </React.Fragment>
          ))}

          {localAssets.length > 0 && (
            <>
              {facts.length > 0 && <span className="text-[#454545]">·</span>}
              <span className="inline-flex items-center gap-1.5">
                <HardDriveDownload size={12} className="text-[#666]" />
                {localAssets.map(([p, label], i) => (
                  <React.Fragment key={label}>
                    {i > 0 && <span className="text-[#454545]">·</span>}
                    <button
                      onClick={() => window.electronAPI.openPath(p)}
                      title={t('openLocal', { label })}
                      className="u-press text-[#9a9a9a] hover:text-white underline-offset-2 hover:underline"
                    >
                      {label}
                    </button>
                  </React.Fragment>
                ))}
              </span>
            </>
          )}
        </div>
      )}
    </div>
  );
}
