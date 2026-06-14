import React, { useState } from 'react';
import { X, Upload, CheckCircle, AlertCircle, Loader } from 'lucide-react';
import { useT } from '../i18n';

export default function ImportModal({ onClose, onImported }) {
  const t = useT('importModal');
  const tc = useT('common');
  const [status, setStatus] = useState('idle');
  const [filePath, setFilePath] = useState(null);
  const [importedCount, setImportedCount] = useState(0);
  const [error, setError] = useState(null);

  const handleChooseFile = async () => {
    const path = await window.electronAPI.openFile();
    if (path) {
      setFilePath(path);
    }
  };

  const handleImport = async () => {
    setStatus('importing');
    try {
      const result = await window.electronAPI.importJSON(filePath);
      setImportedCount(result?.imported ?? 0);
      setStatus('done');
      onImported?.();
    } catch (err) {
      setError(err.message);
      setStatus('error');
    }
  };

  const handleOverlayClick = () => {
    if (status === 'importing') return;
    onClose();
  };

  // Reset to the picker so the user can import a second export without reopening.
  const importAnother = () => {
    setFilePath(null);
    setImportedCount(0);
    setError(null);
    setStatus('idle');
  };

  const fileName = filePath ? filePath.split('/').pop() : null;

  return (
    <div
      data-testid="import-modal"
      className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 u-backdrop-in"
      onClick={handleOverlayClick}
    >
      <div
        className="bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl p-6 w-full max-w-sm shadow-2xl u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div key={status} className="u-swap-in">
          {status === 'idle' && (
            <>
              <div className="flex items-center justify-between mb-4">
                <h2 className="text-white text-lg font-semibold font-display">{t('title')}</h2>
                <button
                  onClick={onClose}
                  className="text-[#888] hover:text-white transition-colors u-press"
                >
                  <X size={18} />
                </button>
              </div>

              <p className="text-[#888] text-sm mb-5 leading-relaxed">{t('desc')}</p>

              {fileName && (
                <div className="mb-4 bg-[#111] border border-[#2e2e2e] rounded-lg px-3 py-2">
                  <code className="text-[#a0a0a0] text-xs break-all">{fileName}</code>
                </div>
              )}

              <div className="flex flex-col gap-2">
                <button
                  onClick={handleChooseFile}
                  className="flex items-center justify-center gap-2 w-full px-4 py-2 rounded-lg border border-[#2e2e2e] text-[#ccc] hover:text-white hover:border-[#444] transition-colors text-sm u-press"
                >
                  <Upload size={15} />
                  {t('chooseFile')}
                </button>

                <button
                  onClick={handleImport}
                  disabled={!filePath}
                  className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors disabled:opacity-30 disabled:cursor-not-allowed u-press"
                >
                  {t('importBtn')}
                </button>

                <button
                  onClick={onClose}
                  className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                >
                  {tc('cancel')}
                </button>
              </div>
            </>
          )}

          {status === 'importing' && (
            <div className="flex flex-col items-center gap-4 py-6">
              <Loader size={32} className="text-white animate-spin" />
              <p className="text-[#ccc] text-sm u-fade-in">{t('importing')}</p>
            </div>
          )}

          {status === 'done' && (
            <>
              <div className="flex flex-col items-center gap-4 py-4 mb-5">
                <CheckCircle size={36} className="text-green-500 u-pop-in" />
                <p
                  className="text-white text-sm font-medium u-fade-in"
                  style={{ animationDelay: '160ms' }}
                >
                  {t('importedCount', { count: importedCount })}
                </p>
              </div>

              <div className="flex flex-col gap-2">
                <button
                  onClick={importAnother}
                  className="w-full px-4 py-2 rounded-lg border border-[#2e2e2e] text-[#ccc] hover:text-white hover:border-[#444] transition-colors text-sm u-press"
                >
                  {t('importAnother')}
                </button>

                <button
                  onClick={onClose}
                  className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors u-press"
                >
                  {tc('close')}
                </button>
              </div>
            </>
          )}

          {status === 'error' && (
            <>
              <div className="flex flex-col items-center gap-3 py-4 mb-5 u-shake">
                <AlertCircle size={36} className="text-red-500 u-pop-in" />
                <p className="text-[#ccc] text-sm text-center">{error}</p>
              </div>

              <div className="flex flex-col gap-2">
                <button
                  onClick={() => {
                    setStatus('idle');
                    setError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors u-press"
                >
                  {t('tryAgain')}
                </button>

                <button
                  onClick={onClose}
                  className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                >
                  {tc('cancel')}
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
