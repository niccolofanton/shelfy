import React from 'react';
import {
  AlertTriangle,
  Ban,
  CheckCircle2,
  Clock,
  ImageOff,
  Loader2,
  RotateCw,
  X,
} from 'lucide-react';
import { useLang, useT } from '../../i18n';
import type { Job } from '../../api/jobs';
import { jobErrorLabel, jobKindLabel, jobStageLabel, jobStateLabel } from './labels';

const STATE_ICON: Record<Job['state'], typeof Clock> = {
  queued: Clock,
  running: Loader2,
  succeeded: CheckCircle2,
  failed: AlertTriangle,
  cancelled: Ban,
};

const STATE_COLOR: Record<Job['state'], string> = {
  queued: '#8a8a8a',
  running: '#7B5CFF',
  succeeded: '#4caf50',
  failed: '#ef5350',
  cancelled: '#8a8a8a',
};

export interface JobRowProps {
  job: Job;
  // Resolved from job.postKey by the view (one batch fetch per page); null
  // once fetched-but-missing (the post is gone), undefined while loading.
  post: Shelfy.Post | null | undefined;
  tileUrl: string | null;
  busy: boolean;
  onOpenPost: (key: string) => void;
  onCancel: (id: number) => void;
  onRetry: (id: number) => void;
}

export default function JobRow({
  job,
  post,
  tileUrl,
  busy,
  onOpenPost,
  onCancel,
  onRetry,
}: JobRowProps): React.JSX.Element {
  const t = useT('jobs');
  const tc = useT('common');
  const { lang } = useLang();
  const StateIcon = STATE_ICON[job.state];
  const canCancel = job.state === 'queued' || job.state === 'running';
  const canRetry = job.state === 'failed' || job.state === 'cancelled';
  const pct = job.progress != null ? Math.round(job.progress * 100) : null;
  const postKey = job.postKey;

  return (
    <div
      data-testid="job-row"
      data-job-id={job.id}
      data-job-state={job.state}
      className="flex items-center gap-3 px-4 py-3 border-b border-[#222] last:border-b-0"
    >
      {postKey &&
        (tileUrl ? (
          <button
            type="button"
            data-testid="job-row-post-link"
            title={t('postLink')}
            aria-label={t('postLink')}
            onClick={() => onOpenPost(postKey)}
            className="shrink-0 w-10 h-10 rounded-md overflow-hidden bg-[#1c1c1c] u-press"
          >
            {/* Decorative: the button already carries the accessible name. */}
            <img src={tileUrl} alt="" className="w-full h-full object-cover" />
          </button>
        ) : (
          <button
            type="button"
            data-testid="job-row-post-link"
            title={t('postLink')}
            aria-label={t('postLink')}
            onClick={() => onOpenPost(postKey)}
            disabled={post === null}
            className="shrink-0 flex items-center justify-center w-10 h-10 rounded-md bg-[#1c1c1c] text-gray-600 u-press disabled:opacity-50"
          >
            <ImageOff size={16} />
          </button>
        ))}

      <div className="flex-1 min-w-0 flex flex-col gap-1">
        <div className="flex items-center gap-2 min-w-0">
          <span className="truncate text-[13px] font-medium text-[#ececec]">
            {jobKindLabel(lang, job.kind)}
          </span>
          {post?.authorUsername && (
            <span className="truncate text-[11.5px] text-[#777]">@{post.authorUsername}</span>
          )}
        </div>
        <div
          className="flex items-center gap-1.5 text-[11.5px]"
          style={{ color: STATE_COLOR[job.state] }}
        >
          <StateIcon size={12} className={job.state === 'running' ? 'u-spin' : undefined} />
          <span>{jobStateLabel(lang, job.state)}</span>
          {job.stage && <span className="text-[#7a7a7a]">· {jobStageLabel(lang, job.stage)}</span>}
          {pct != null && job.state === 'running' && (
            <span className="tabular-nums text-[#6b6b6b]">{pct}%</span>
          )}
          {job.maxAttempts > 1 && (
            <span className="text-[#6b6b6b]">
              · {t('tries', { attempts: job.attempts, max: job.maxAttempts })}
            </span>
          )}
        </div>
        {job.state === 'running' && pct != null && (
          <div className="h-[3px] rounded-full bg-[#242424] overflow-hidden">
            <div
              className="h-full rounded-full u-progress"
              style={{ width: `${Math.max(2, pct)}%`, background: '#7B5CFF' }}
            />
          </div>
        )}
        {job.state === 'failed' && job.errorCode && (
          <p
            data-testid="job-row-error"
            className="text-[11.5px] text-[#d77] truncate"
            title={jobErrorLabel(lang, job.errorCode)}
          >
            {jobErrorLabel(lang, job.errorCode)}
          </p>
        )}
      </div>

      <div className="shrink-0 flex items-center gap-1.5">
        {canCancel && (
          <button
            type="button"
            data-testid="job-row-cancel"
            disabled={busy}
            onClick={() => onCancel(job.id)}
            title={t('cancelJob')}
            aria-label={t('cancelJob')}
            className="flex items-center justify-center w-7 h-7 rounded-md text-[#9a9a9a] hover:text-white hover:bg-[#222] u-press disabled:opacity-50"
          >
            <X size={14} />
          </button>
        )}
        {canRetry && (
          <button
            type="button"
            data-testid="job-row-retry"
            disabled={busy}
            onClick={() => onRetry(job.id)}
            title={t('retryJob')}
            className="flex items-center gap-1.5 h-7 px-2.5 rounded-md bg-[#1f1f1f] text-[12px] text-[#ddd] hover:bg-[#272727] u-press disabled:opacity-50"
          >
            <RotateCw size={12} /> {tc('retry')}
          </button>
        )}
      </div>
    </div>
  );
}
