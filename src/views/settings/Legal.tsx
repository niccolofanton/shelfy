// Settings → Legal on a server (plan §7.2; P1-20): the disclaimer and the
// privacy notice, each with the acceptance the account recorded
// (`POST /me/consent`, through the consent gate). The desktop keeps its own
// card (LegalCard in ../Settings.tsx), which reads localStorage.
import React, { useEffect, useState } from 'react';
import { Scale, ShieldCheck, X } from 'lucide-react';
import type { AccountApi } from '../../api/account';
import DisclaimerGate from '../../components/DisclaimerGate';
import PrivacyNotice from '../../components/PrivacyNotice';
import { DISCLAIMER_VERSION, PRIVACY_VERSION } from '../../disclaimer';
import { useLang, useT } from '../../i18n';
import { formatDateTime } from './format';
import { Card, CardHeader } from './ui';

function PrivacyDialog({ onClose }: { onClose: () => void }): React.JSX.Element {
  const t = useT('privacy');
  const tc = useT('common');

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div
      data-testid="privacy-dialog"
      role="dialog"
      aria-modal="true"
      aria-label={t('title')}
      className="fixed inset-0 bg-black/90 flex items-center justify-center z-[100] p-4 sm:p-6 u-backdrop-in"
      onClick={onClose}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-lg overflow-hidden u-dialog-in flex flex-col max-h-[88vh]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-6 h-14 border-b border-[#2e2e2e] shrink-0">
          <ShieldCheck size={18} className="text-[#7B5CFF]" />
          <span className="flex-1 text-white text-sm font-semibold font-display">{t('title')}</span>
          <button
            type="button"
            onClick={onClose}
            aria-label={tc('close')}
            className="u-press rounded-md p-1 text-gray-500 hover:text-white"
          >
            <X size={16} />
          </button>
        </div>
        <div className="px-6 py-5 overflow-y-auto">
          <PrivacyNotice />
        </div>
        <div className="px-6 py-4 border-t border-[#2e2e2e] shrink-0">
          <button
            type="button"
            onClick={onClose}
            className="w-full h-10 rounded-lg bg-[#2a2a2a] hover:bg-[#333] text-white text-sm font-medium transition-colors u-press"
          >
            {tc('close')}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function LegalSection({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const { lang } = useLang();
  const [open, setOpen] = useState<'disclaimer' | 'privacy' | null>(null);
  const consent = account.consent();
  const disclaimerAt = consent.disclaimerAcceptedAt;
  const privacyAt = consent.privacyAcceptedAt;

  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-4 items-start" data-testid="settings-legal">
      <Card testId="legal-disclaimer">
        <CardHeader icon={Scale} title={t('legalTitle')} description={t('legalDescWeb')} />
        <p className="text-gray-600 text-xs mt-3" data-testid="legal-disclaimer-status">
          {disclaimerAt && consent.disclaimerVersion
            ? t('legalAccepted', {
                date: formatDateTime(disclaimerAt, lang),
                version: consent.disclaimerVersion,
              })
            : t('legalNotAccepted', { version: DISCLAIMER_VERSION })}
        </p>
        <button
          type="button"
          data-testid="legal-disclaimer-read"
          onClick={() => setOpen('disclaimer')}
          className="mt-3 text-[#7B5CFF] text-xs font-medium hover:underline u-press"
        >
          {t('legalReview')}
        </button>
      </Card>

      <Card testId="legal-privacy">
        <CardHeader icon={ShieldCheck} title={t('privacyTitle')} description={t('privacyDesc')} />
        <p className="text-gray-600 text-xs mt-3" data-testid="legal-privacy-status">
          {privacyAt && consent.privacyVersion
            ? t('privacyAccepted', {
                date: formatDateTime(privacyAt, lang),
                version: consent.privacyVersion,
              })
            : t('privacyNotAccepted', { version: PRIVACY_VERSION })}
        </p>
        <button
          type="button"
          data-testid="legal-privacy-read"
          onClick={() => setOpen('privacy')}
          className="mt-3 text-[#7B5CFF] text-xs font-medium hover:underline u-press"
        >
          {t('privacyRead')}
        </button>
      </Card>

      {open === 'disclaimer' && (
        <DisclaimerGate
          mode="review"
          acceptance={
            disclaimerAt && consent.disclaimerVersion
              ? { acceptedAt: disclaimerAt, version: consent.disclaimerVersion }
              : null
          }
          onClose={() => setOpen(null)}
        />
      )}
      {open === 'privacy' && <PrivacyDialog onClose={() => setOpen(null)} />}
    </div>
  );
}
