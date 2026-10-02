import React from 'react';
import { PRIVACY_VERSION } from '../disclaimer';
import { useT } from '../i18n';

// The privacy notice of the web app, version PRIVACY_VERSION: its text is the
// `privacy` i18n namespace (src/i18n/messages/privacy.ts). Shown inside the
// consent gate and in Settings → Legal.
const SECTIONS: { name: string; items?: number }[] = [
  { name: 'who' },
  { name: 'data', items: 5 },
  { name: 'where' },
  { name: 'processors', items: 4 },
  { name: 'tracking' },
  { name: 'retention', items: 6 },
  { name: 'rights' },
  { name: 'others' },
  { name: 'changes' },
];

export default function PrivacyNotice({
  showTitle = true,
}: {
  showTitle?: boolean;
}): React.JSX.Element {
  const t = useT('privacy');
  return (
    <div
      data-testid="privacy-notice"
      className="space-y-4 text-[13px] leading-relaxed text-[#b8b8b8]"
    >
      {showTitle && (
        <div>
          <h2 className="font-display text-sm font-semibold text-white">{t('title')}</h2>
          <p className="text-[11px] text-gray-500">{t('version', { version: PRIVACY_VERSION })}</p>
        </div>
      )}
      <p>{t('intro')}</p>
      {SECTIONS.map(({ name, items }) => (
        <section key={name}>
          <h3 className="mb-1 text-[13px] font-semibold text-gray-200">{t(`${name}Title`)}</h3>
          {items ? (
            <ul className="list-disc space-y-1 pl-5 marker:text-[#7B5CFF]">
              {Array.from({ length: items }, (_, i) => (
                <li key={i}>{t(`${name}${i + 1}`)}</li>
              ))}
            </ul>
          ) : (
            <p>{t(`${name}Body`)}</p>
          )}
        </section>
      ))}
    </div>
  );
}
