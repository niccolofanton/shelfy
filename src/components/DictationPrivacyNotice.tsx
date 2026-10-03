import React, { useId } from 'react';
import { createPortal } from 'react-dom';
import { useDialog } from '../hooks/useDialog';
import { useT } from '../i18n';
export default function DictationPrivacyNotice({
  open,
  onAccept,
  onCancel,
}: {
  open: boolean;
  onAccept?: () => Promise<void>;
  onCancel?: () => void;
}): React.JSX.Element | null {
  const t = useT('dictation');
  const title = useId();
  const dialog = useDialog<HTMLDivElement>({ open, onClose: () => onCancel?.() });
  if (!open) return null;
  return createPortal(
    <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/70 p-4">
      <section
        ref={dialog}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title}
        className="max-w-md rounded-xl border border-[#303030] bg-[#161616] p-5 text-gray-200"
      >
        <h2 id={title} className="font-semibold">
          {t('interimTitle')}
        </h2>
        <p className="my-4 text-sm">{t('interimNotice')}</p>
        <div className="flex justify-end gap-4">
          <button type="button" className="min-h-11 min-w-11 px-3" onClick={() => onCancel?.()}>
            {t('interimCancel')}
          </button>
          <button
            type="button"
            className="min-h-11 min-w-11 rounded bg-violet-600 px-3 py-2 text-sm"
            onClick={() => void onAccept?.()}
          >
            {t('interimAccept')}
          </button>
        </div>
      </section>
    </div>,
    document.body,
  );
}
