import React from 'react';
import { Languages } from 'lucide-react';
import { useLang, useT, LANGUAGES } from '../i18n';

// Settings card that switches the UI language. Mirrors the styling of the other
// settings cards (UpdateChannelPicker, ConcurrencyPicker). The choice is applied
// app-wide instantly via the i18n context and persisted in localStorage.
export default function LanguageCard() {
  const { lang, setLang } = useLang();
  const t = useT('language');

  return (
    <div className="rounded-xl border border-[#242424] bg-[#161616] p-5">
      <div className="flex items-start gap-3">
        <Languages size={18} className="text-gray-400 mt-0.5 shrink-0" />
        <div className="flex-1 min-w-0">
          <p className="text-white text-sm font-medium">{t('title')}</p>
          <p className="text-gray-500 text-xs mt-1 leading-relaxed">{t('desc')}</p>
        </div>
        <select
          value={lang}
          onChange={(e) => setLang(e.target.value)}
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
    </div>
  );
}
