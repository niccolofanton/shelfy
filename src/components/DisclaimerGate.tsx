import React, { useEffect, useState } from 'react';
import { Scale, AlertTriangle, Check, ChevronDown, ChevronUp } from 'lucide-react';
import disclaimerText from '../../DISCLAIMER.md?raw';
import { acceptDisclaimer, getDisclaimerAcceptance } from '../disclaimer';
import { useT, useLang, localeTag } from '../i18n';

// Strip the leading Markdown heading characters so the embedded text reads
// cleanly in the <pre> block (we don't ship a full Markdown renderer).
const fullText = disclaimerText.replace(/^#{1,6}\s+/gm, '').replace(/^>\s?/gm, '');

type DisclaimerMode = 'gate' | 'review';

interface DisclaimerGateProps {
  mode?: DisclaimerMode;
  onAccept?: () => void;
  onClose?: () => void;
}

/**
 * DisclaimerGate — first-run legal acknowledgement.
 *
 * Two modes:
 *   - 'gate'   (default): blocking full-screen overlay shown until the user
 *              ticks the box and accepts. Cannot be dismissed by Esc/backdrop.
 *              On accept, persists acceptance (version + timestamp +
 *              "don't show again") and calls onAccept().
 *   - 'review': dismissible read-only view (from Settings → Note legali),
 *              showing the same text plus the recorded acceptance.
 *
 * Props:
 *   mode      — 'gate' | 'review'  (default 'gate')
 *   onAccept()— fired after acceptance is persisted (gate mode)
 *   onClose() — dismiss (review mode)
 */
export default function DisclaimerGate({
  mode = 'gate',
  onAccept,
  onClose,
}: DisclaimerGateProps): React.ReactElement {
  const t = useT('disclaimer');
  const tc = useT('common');
  const { lang } = useLang();
  const [checked, setChecked] = useState(false);
  // Default ON so accepting hides the notice from then on; the user can untick
  // it to keep seeing the reminder at every launch.
  const [dontShowAgain, setDontShowAgain] = useState(true);
  const [showFull, setShowFull] = useState(false);
  const isReview = mode === 'review';
  const accepted = isReview ? getDisclaimerAcceptance() : null;

  // In review mode, Esc/backdrop dismiss. The gate is intentionally non-dismissible.
  useEffect(() => {
    if (!isReview) return undefined;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onClose?.();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isReview, onClose]);

  const handleAccept = (): void => {
    if (!checked) return;
    acceptDisclaimer(dontShowAgain);
    onAccept?.();
  };

  return (
    <div
      data-testid="disclaimer-gate"
      className="fixed inset-0 bg-black/90 flex items-center justify-center z-[100] p-6 u-backdrop-in"
      onClick={isReview ? onClose : undefined}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-lg overflow-hidden u-dialog-in flex flex-col max-h-[88vh]"
        onClick={(e: React.MouseEvent<HTMLDivElement>) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-6 h-14 border-b border-[#2e2e2e] shrink-0">
          <Scale size={18} className="text-[#7B5CFF]" />
          <span className="text-white text-sm font-semibold font-display">{t('title')}</span>
        </div>

        <div className="px-6 py-5 overflow-y-auto space-y-4 text-sm leading-relaxed text-[#b8b8b8]">
          <div className="flex items-start gap-2 rounded-lg bg-[#7B5CFF]/10 border border-[#7B5CFF]/30 px-3 py-2.5">
            <AlertTriangle size={16} className="text-[#7B5CFF] mt-0.5 shrink-0" />
            <p className="text-[#d9d2ff]">
              {t('bannerPre')}
              <strong>{t('bannerOnly')}</strong>
              {t('bannerMid')}
              <strong>{t('bannerYou')}</strong>
              {t('bannerMid2')}
              <strong>{t('bannerYour')}</strong>
              {t('bannerMid3')}
              <strong>{t('bannerPersonal')}</strong>
              {t('bannerMid4')}
              <strong>{t('bannerRisk')}</strong>
              {t('bannerEnd')}
            </p>
          </div>

          <ul className="space-y-2.5 list-disc pl-5 marker:text-[#7B5CFF]">
            <li>
              <strong className="text-white">{t('tosTitle')}</strong>
              {t('tosBody1')}
              <strong>{t('tosViolate')}</strong>
              {t('tosBody2')}
              <strong>{t('tosSuspend')}</strong>
              {t('tosBody3')}
            </li>
            <li>
              <strong className="text-white">{t('copyrightTitle')}</strong>
              {t('copyrightBody')}
            </li>
            <li>
              <strong className="text-white">{t('privacyTitle')}</strong>
              {t('privacyBody1')}
              <strong>{t('privacyNo')}</strong>
              {t('privacyBody2')}
            </li>
            <li>
              <strong className="text-white">{t('warrantyTitle')}</strong>
              {t('warrantyBody1')}
              <strong>{t('warrantyIndemnify')}</strong>
              {t('warrantyBody2')}
            </li>
            <li>
              <strong className="text-white">{t('affiliationTitle')}</strong>
              {t('affiliationBody')}
            </li>
          </ul>

          <div className="border-t border-[#2e2e2e] pt-3">
            <button
              onClick={() => setShowFull((v) => !v)}
              data-testid="disclaimer-toggle-full"
              className="flex items-center gap-1.5 text-[#a59bff] text-xs font-medium hover:underline u-press"
            >
              {showFull ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
              {showFull ? t('hideFull') : t('showFull')}
            </button>
            {showFull && (
              <pre
                data-testid="disclaimer-full-text"
                className="u-fade-in mt-3 max-h-64 overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-[#111] border border-[#262626] p-3 text-[11px] leading-relaxed text-[#9a9a9a] font-mono"
              >
                {fullText}
              </pre>
            )}
          </div>

          {isReview && accepted && (
            <p
              data-testid="disclaimer-accepted-info"
              className="text-[#6f6f6f] text-xs border-t border-[#2e2e2e] pt-3"
            >
              {t('acceptedOn', {
                date: new Date(accepted.acceptedAt).toLocaleString(localeTag(lang), {
                  dateStyle: 'medium',
                  timeStyle: 'short',
                }),
                version: accepted.version,
              })}
            </p>
          )}
        </div>

        <div className="px-6 py-4 border-t border-[#2e2e2e] shrink-0 space-y-3">
          {isReview ? (
            <button
              onClick={onClose}
              className="w-full h-10 rounded-lg bg-[#2a2a2a] hover:bg-[#333] text-white text-sm font-medium transition-colors u-press"
            >
              {tc('close')}
            </button>
          ) : (
            <>
              <label className="flex items-start gap-2.5 cursor-pointer select-none">
                <span
                  className={`mt-0.5 flex items-center justify-center w-5 h-5 rounded border shrink-0 transition-colors ${
                    checked
                      ? 'bg-[#7B5CFF] border-[#7B5CFF]'
                      : 'bg-transparent border-[#444] hover:border-[#666]'
                  }`}
                >
                  {checked && <Check size={14} className="text-white" />}
                </span>
                <input
                  type="checkbox"
                  data-testid="disclaimer-checkbox"
                  className="sr-only"
                  checked={checked}
                  onChange={(e: React.ChangeEvent<HTMLInputElement>) =>
                    setChecked(e.target.checked)
                  }
                />
                <span className="text-sm text-[#cfcfcf]">{t('checkboxAccept')}</span>
              </label>

              <label className="flex items-center gap-2.5 cursor-pointer select-none pl-[2px]">
                <span
                  className={`flex items-center justify-center w-[18px] h-[18px] rounded border shrink-0 transition-colors ${
                    dontShowAgain
                      ? 'bg-[#3a3a3a] border-[#555]'
                      : 'bg-transparent border-[#444] hover:border-[#666]'
                  }`}
                >
                  {dontShowAgain && <Check size={12} className="text-[#cfcfcf]" />}
                </span>
                <input
                  type="checkbox"
                  data-testid="disclaimer-dont-show"
                  className="sr-only"
                  checked={dontShowAgain}
                  onChange={(e: React.ChangeEvent<HTMLInputElement>) =>
                    setDontShowAgain(e.target.checked)
                  }
                />
                <span className="text-xs text-[#9a9a9a]">{t('dontShowAgain')}</span>
              </label>

              <button
                onClick={handleAccept}
                disabled={!checked}
                data-testid="disclaimer-accept"
                className="w-full h-10 rounded-lg bg-[#7B5CFF] hover:bg-[#6a4bf0] text-white text-sm font-semibold transition-colors u-press disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {t('acceptAndContinue')}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
