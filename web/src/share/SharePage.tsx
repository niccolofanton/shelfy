import React, { useEffect, useMemo, useState } from 'react';
import { Link } from 'wouter';
import { CheckCircle2, Loader2 } from 'lucide-react';
import { useFailureText } from '@ui/hooks/useFailureText';
import { useT } from '@ui/i18n';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import AuthLayout, { Notice, PRIMARY_BUTTON } from '../auth/AuthLayout';
import { extractSharedUrl } from './extractUrl';

type Status =
  | { state: 'saving' }
  | { state: 'saved'; key: string; created: boolean }
  | { state: 'error'; message: string };

// `/share` (plan §2.17, §2.19 Routes; contract C7): where Android's share
// sheet, the iOS Shortcut and the bookmarklet land. Rendered once signed in
// (web/src/Root.tsx), the same way as DevicePage — a web-only utility page,
// not part of the shared App, so it needs none of the library chrome around
// it. Signed out, Root sends the user to `/login?next=/share?…` first and
// comes back here with the same query once they are.
export default function SharePage({
  client,
  url,
  text,
  title,
}: {
  client: ShelfyClient;
  url: string | null;
  text: string | null;
  title: string | null;
}): React.JSX.Element {
  const t = useT('share');
  const te = useT('errors');
  const failure = useFailureText();
  const sharedUrl = useMemo(() => extractSharedUrl({ url, text, title }), [url, text, title]);
  const [status, setStatus] = useState<Status | null>(sharedUrl ? { state: 'saving' } : null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!sharedUrl) return undefined;
    if (!client.links) {
      setStatus({ state: 'error', message: te('unavailable') });
      return undefined;
    }
    let alive = true;
    setStatus({ state: 'saving' });
    client.links
      .create(sharedUrl)
      .then((result) => {
        if (alive) setStatus({ state: 'saved', key: result.key, created: result.created });
      })
      .catch((err: unknown) => {
        if (alive) setStatus({ state: 'error', message: failure(err) });
      });
    return () => {
      alive = false;
    };
    // `attempt` (bumped by the retry button below) is a dependency only to
    // retrigger this effect; its value carries no data.
  }, [client, sharedUrl, failure, te, attempt]);

  if (!sharedUrl || !status) {
    return (
      <AuthLayout title={t('title')}>
        <div className="space-y-4">
          <Notice tone="error" testId="share-no-link">
            {t('noLink')}
          </Notice>
          <Link href="/" className={PRIMARY_BUTTON} data-testid="share-back">
            {te('backToLibrary')}
          </Link>
        </div>
      </AuthLayout>
    );
  }

  if (status.state === 'error') {
    return (
      <AuthLayout title={t('title')}>
        <div className="space-y-4" data-testid="share-error">
          <Notice tone="error">{status.message}</Notice>
          <button
            type="button"
            data-testid="share-retry"
            onClick={() => setAttempt((n) => n + 1)}
            className={PRIMARY_BUTTON}
          >
            {t('retry')}
          </button>
          <Link
            href="/"
            className="u-press block text-center text-[13px] text-gray-500 hover:text-white"
            data-testid="share-back"
          >
            {te('backToLibrary')}
          </Link>
        </div>
      </AuthLayout>
    );
  }

  if (status.state === 'saved') {
    return (
      <AuthLayout title={t('title')}>
        <div className="space-y-4" data-testid="share-saved">
          <p role="status" className="flex items-start gap-2 text-sm leading-relaxed text-gray-300">
            <CheckCircle2 size={16} className="mt-0.5 shrink-0 text-emerald-400" />
            {status.created ? t('saved') : t('alreadySaved')}
          </p>
          <Link
            href={`/p/${encodeURIComponent(status.key)}`}
            className={PRIMARY_BUTTON}
            data-testid="share-open"
          >
            {t('open')}
          </Link>
        </div>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout title={t('title')}>
      <div
        className="flex items-center justify-center gap-2 py-6 text-sm text-gray-400"
        data-testid="share-saving"
      >
        <Loader2 size={16} className="animate-spin" />
        {t('saving')}
      </div>
    </AuthLayout>
  );
}
