import React from 'react';
import { TOKEN_TTL_OPTIONS } from '../../api/account';
import { useT } from '../../i18n';
import { INPUT } from './ui';
export default function TokenExpirySelect({
  value,
  onChange,
  disabled,
  testId,
}: {
  value: number;
  onChange: (days: number) => void;
  disabled?: boolean;
  testId: string;
}): React.JSX.Element {
  const t = useT('settings');
  return (
    <label className="space-y-1">
      <span className="block text-[11px] font-medium text-gray-400">{t('tokenExpiryLabel')}</span>
      <select
        className={INPUT}
        data-testid={testId}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(Number(e.target.value))}
      >
        {TOKEN_TTL_OPTIONS.map((days) => (
          <option key={days} value={days}>
            {t('tokenExpiryDays', { days })}
          </option>
        ))}
      </select>
    </label>
  );
}
