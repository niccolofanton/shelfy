import { createPortal } from 'react-dom';
import { useDialog } from '../../hooks/useDialog';
import { useNavigation } from '../../api/navigation';
import { useT } from '../../i18n';
import { Button, Notice } from '../ui';

export default function SyncHelp({ code, onClose }: { code: string; onClose: () => void }) {
  const t = useT('gallery');
  const navigation = useNavigation();
  const ref = useDialog({ onClose });
  const key = `webSync_${code}`;
  const text = t(key);
  return createPortal(
    <div
      className="fixed inset-0 z-modal flex items-center justify-center bg-black/70 p-4"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby="sync-help-title"
        tabIndex={-1}
        data-testid="sync-help"
        className="w-full max-w-sm rounded-xl border border-strong bg-panel p-5 shadow-2xl"
      >
        <h2 id="sync-help-title" className="mb-3 text-base text-primary">
          {t('webSyncHelpTitle')}
        </h2>
        <Notice
          tone={['mobile', 'missing', 'unsupported', 'not_paired'].includes(code) ? 'info' : 'warn'}
        >
          {text === key ? t('webSyncError', { code }) : text}
        </Notice>
        <div className="mt-4 flex flex-wrap gap-2">
          <Button
            onClick={() => {
              navigation?.navigate({ name: 'settings', section: 'connections' });
              onClose();
            }}
          >
            {t('webSyncConnections')}
          </Button>
          <Button variant="ghost" onClick={onClose}>
            {t('webSyncClose')}
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
