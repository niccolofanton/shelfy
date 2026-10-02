import React, { useState } from 'react';
import { AlertTriangle, Languages } from 'lucide-react';
import { useShelfy } from '../api/ShelfyProvider';
import type { AccountLanguage } from '../api/account';
import { useLang, useT, LANGUAGES } from '../i18n';

// Settings card that switches the UI language. Mirrors the styling of the other
// settings cards (UpdateChannelPicker, ConcurrencyPicker). The choice is applied
// app-wide instantly via the i18n context and persisted in localStorage; on the
// web it is saved to the account too (`PUT /me/settings`), so it follows the
// user to every browser where they sign in.
export default function LanguageCard(): React.ReactElement {
  const { lang, setLang } = useLang();
  const t = useT('language');
  const account = useShelfy().account;
  const [saveFailed, setSaveFailed] = useState(false);

  const choose = (next: string): void => {
    const option = LANGUAGES.find((l) => l.code === next);
    if (!option) return;
    setLang(option.code);
    if (!account) return;
    setSaveFailed(false);
    account
      .updateSettings({ language: option.code as AccountLanguage })
      .catch(() => setSaveFailed(true));
  };

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <Languages size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <p className="text-white text-sm font-medium">{t('title')}</p>
          <p className="text-gray-500 text-xs mt-1 leading-relaxed">
            {t('desc')}
            {account && ` ${t('descAccount')}`}
          </p>
        </div>
        <select
          value={lang}
          data-testid="language-select"
          onChange={(e: React.ChangeEvent<HTMLSelectElement>) => choose(e.target.value)}
          aria-label={t('title')}
          className="bg-[#1c1c1c] border border-[#333] text-white text-sm rounded-lg px-3 py-2 shrink-0 focus:outline-none focus:border-[var(--accent)]"
        >
          {LANGUAGES.map((l) => (
            <option key={l.code} value={l.code}>
              {l.label}
            </option>
          ))}
        </select>
      </div>
      {saveFailed && (
        <p
          role="alert"
          data-testid="language-save-error"
          className="u-fade-in mt-3 flex items-center gap-1.5 text-xs text-red-400"
        >
          <AlertTriangle size={13} className="shrink-0" /> {t('saveFailed')}
        </p>
      )}
    </div>
  );
}
