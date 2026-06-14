import React, { useEffect, useRef, useState } from 'react';
import { X, MessageSquare, AlertCircle, Loader, CheckCircle2, Paperclip } from 'lucide-react';
import { useT } from '../i18n';

// Limiti allegati: solo immagini, max 5 file da 5 MB l'uno (post-encoding base64
// l'email resta ben sotto il limite di Resend).
const MAX_FILES = 5;
const MAX_FILE_BYTES = 5 * 1024 * 1024;
// Cap aggregato sul base64, allineato a quello applicato dal main process
// (feedback.js: MAX_TOTAL_B64 = 16 MB). Va rispettato anche QUI, altrimenti il
// main scarta silenziosamente gli allegati eccedenti e l'utente crede di averli
// inviati tutti. Stimiamo il base64 di un file binario come ceil(size/3)*4.
const MAX_TOTAL_B64 = 16 * 1024 * 1024;
const estimateB64 = (bytes: number): number => Math.ceil(bytes / 3) * 4;

// Uno screenshot allegato, già letto come base64 + data URL per l'anteprima.
interface Attachment {
  id: number;
  name: string;
  type: string;
  size: number;
  dataUrl: string;
  base64: string;
}

// Delta dei FileReader ancora in volo (numero + base64 stimato).
interface PendingDelta {
  count: number;
  b64: number;
}

// Motivi distinti dello scarto di un batch di allegati.
interface AttachRejectReasons {
  tooBigName: string | null;
  tooMany: boolean;
  tooHeavy: boolean;
}

// Overlay di conferma interno.
type ConfirmKind = 'send' | 'close';
// Stato dell'invio.
type FeedbackStatus = 'idle' | 'sending' | 'sent' | 'error';

interface FeedbackModalProps {
  onClose: () => void;
}

// `inert` is a valid HTML attribute used to make the form behind an overlay
// non-interactive, but it isn't declared on the stable @types/react
// HTMLAttributes. Mirror the JS exactly: present (empty string) when active,
// omitted otherwise — without resorting to `any`.
type InertProp = { inert?: '' };
const inertProp = (active: boolean): InertProp => (active ? { inert: '' } : {});

/**
 * FeedbackModal — scrivi un messaggio di feedback che viene inviato via email
 * direttamente dall'app (main process → Resend), senza aprire client esterni.
 *
 * Comportamento richiesto:
 *   - Il click sullo sfondo (overlay) NON chiude il dialog.
 *   - Esc / X / Annulla: se ci sono modifiche in sospeso, chiedono conferma.
 *   - "Invia" chiede sempre una conferma prima dell'invio.
 *
 * Props:
 *   onClose() — chiude il modal.
 */
export default function FeedbackModal({ onClose }: FeedbackModalProps) {
  const t = useT('feedback');
  const tc = useT('common');
  const [message, setMessage] = useState<string>('');
  // Screenshot allegati: { id, name, type, size, dataUrl, base64 }.
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [attachWarn, setAttachWarn] = useState<string | null>(null);
  // Overlay di conferma interno: null | 'send' | 'close'.
  const [confirm, setConfirm] = useState<ConfirmKind | null>(null);
  // Stato dell'invio: idle | sending | sent | error.
  const [status, setStatus] = useState<FeedbackStatus>('idle');
  const [error, setError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const overlayRef = useRef<HTMLDivElement | null>(null);
  const idRef = useRef<number>(0);
  // Contabilità "live" degli allegati condivisa tra invocazioni concorrenti di
  // addFiles: i FileReader risolvono in modo async, e leggere lo stato catturato
  // nella closure produrrebbe snapshot stale. attachmentsRef rispecchia lo stato
  // committato; pendingRef tiene il delta dei reader ancora in volo (numero +
  // base64 stimato). L'uso effettivo è committato + pending: così due paste
  // ravvicinati non oltrepassano i cap.
  const attachmentsRef = useRef<Attachment[]>([]);
  const pendingRef = useRef<PendingDelta>({ count: 0, b64: 0 });

  // Lunghezza massima del messaggio, allineata al cap del main process
  // (feedback.js): così il limite è visibile PRIMA dell'invio.
  const MAX_MESSAGE_LEN = 5000;

  const isDirty = message.trim() !== '' || attachments.length > 0;
  const canSend = (message.trim() !== '' || attachments.length > 0) && status !== 'sending';
  const busy = status === 'sending';

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  // Auto-chiusura dopo un invio riuscito: il timer è legato allo stato 'sent'
  // e viene ripulito allo smontaggio per non chiamare onClose su un componente
  // già smontato (stale callback / leaked timer).
  useEffect(() => {
    if (status !== 'sent') return undefined;
    const id = setTimeout(() => onClose(), 1400);
    return () => clearTimeout(id);
  }, [status, onClose]);

  // Mantiene attachmentsRef allineato allo stato committato. È un mirror passivo:
  // gli updater di setAttachments aggiornano il ref in modo SINCRONO (vedi sotto)
  // così la contabilità è coerente anche tra un commit e l'altro; questo effect
  // copre i percorsi che non passano dagli updater interni (es. una rimozione).
  useEffect(() => {
    attachmentsRef.current = attachments;
  }, [attachments]);

  // Aggiunge file immagine (da picker o incolla), leggendoli come base64.
  // I controlli su dimensione, numero massimo e peso aggregato sono valutati QUI
  // (fuori dagli updater di stato) così da non eseguire side effect dentro un
  // reducer; i motivi distinti vengono accumulati per mostrare TUTTE le
  // avvertenze rilevanti, non solo l'ultima incontrata.
  const addFiles = (files: FileList | File[] | null | undefined): void => {
    const imgs = Array.from(files || []).filter(
      (f): f is File => !!f && (f.type?.startsWith('image/') ?? false),
    );
    if (imgs.length === 0) return;

    // Motivi distinti dello scarto: vengono composti in un unico avviso a fine
    // ciclo, così l'utente vede sia i file troppo grandi sia il superamento del
    // numero massimo / del peso aggregato.
    let tooBigName: string | null = null;
    let tooMany = false;
    let tooHeavy = false;

    // Contabilità live = committato (stato reale) + pending (reader in volo),
    // letti dai ref e non da uno snapshot stale catturato nella closure.
    let count = attachmentsRef.current.length + pendingRef.current.count;
    let b64 =
      attachmentsRef.current.reduce((s, a) => s + (a.base64?.length || 0), 0) +
      pendingRef.current.b64;

    const accepted: File[] = [];
    for (const file of imgs) {
      if (file.size > MAX_FILE_BYTES) {
        tooBigName = file.name || t('unnamedImage');
        continue;
      }
      if (count >= MAX_FILES) {
        tooMany = true;
        continue;
      }
      const est = estimateB64(file.size);
      if (b64 + est > MAX_TOTAL_B64) {
        // Sforerebbe il cap aggregato del main: scartarlo qui (con avviso) evita
        // lo scarto silenzioso lato main e il falso "inviato".
        tooHeavy = true;
        continue;
      }
      count += 1;
      b64 += est;
      // Riserva subito slot e budget come pending: invocazioni ravvicinate (es.
      // due paste consecutivi) vedono la prenotazione e non riaccettano oltre i cap.
      pendingRef.current = {
        count: pendingRef.current.count + 1,
        b64: pendingRef.current.b64 + est,
      };
      accepted.push(file);
    }

    setAttachWarn(composeAttachWarn({ tooBigName, tooMany, tooHeavy }));

    // Libera la prenotazione pending di un file (lettura fallita o guard di
    // sicurezza scattato).
    const releasePending = (est: number): void => {
      pendingRef.current = {
        count: Math.max(0, pendingRef.current.count - 1),
        b64: Math.max(0, pendingRef.current.b64 - est),
      };
    };

    for (const file of accepted) {
      const est = estimateB64(file.size);
      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = String(reader.result || '');
        const base64 = dataUrl.split(',')[1] || '';
        if (!base64) {
          releasePending(est);
          return;
        }
        idRef.current += 1;
        const id = idRef.current;
        let rejected = false;
        setAttachments((cur) => {
          // Guard di sicurezza puro rispetto al cap (gli onload sono asincroni e
          // concorrenti). Aggiorniamo attachmentsRef in modo sincrono insieme allo
          // stato così committato + pending resta coerente anche tra i commit.
          if (cur.length >= MAX_FILES) {
            rejected = true;
            return cur;
          }
          const next = [
            ...cur,
            {
              id,
              name: file.name || `screenshot-${id}.png`,
              type: file.type || 'image/png',
              size: file.size,
              dataUrl,
              base64,
            },
          ];
          attachmentsRef.current = next;
          return next;
        });
        // Il file lascia la coda pending in ogni caso: se accettato ora vive nello
        // stato (e in attachmentsRef); se rifiutato dal guard, va segnalato così lo
        // scarto non è silenzioso.
        releasePending(est);
        if (rejected) setAttachWarn(t('tooManyFiles', { max: MAX_FILES }));
      };
      reader.onerror = () => releasePending(est);
      reader.readAsDataURL(file);
    }
  };

  // Compone l'avviso allegati dai motivi raccolti, riusando le stringhe i18n
  // esistenti. Possono coesistere più righe (file troppo grande + troppi file /
  // peso eccessivo): le uniamo così nessun motivo va perso.
  const composeAttachWarn = ({
    tooBigName,
    tooMany,
    tooHeavy,
  }: AttachRejectReasons): string | null => {
    const lines: string[] = [];
    if (tooBigName) lines.push(t('fileTooBig', { name: tooBigName }));
    // Distinguish the two causes: too MANY files vs. exceeding the aggregate WEIGHT
    // cap. Folding both into "max N images" misreports a weight rejection as a count
    // one; both can also occur in the same batch.
    if (tooMany) lines.push(t('tooManyFiles', { max: MAX_FILES }));
    if (tooHeavy) lines.push(t('attachTooHeavy'));
    return lines.length ? lines.join('\n') : null;
  };

  const removeAttachment = (id: number): void =>
    setAttachments((cur) => cur.filter((a) => a.id !== id));

  // È attivo un overlay (conferma o esito invio) sopra al form? In tal caso il
  // focus trap e l'inerzia vanno confinati al solo overlay.
  const overlayActive = confirm != null || status === 'sent';

  // Focus trap basilare: Tab/Shift+Tab restano dentro al pannello, così il
  // focus non finisce sui controlli dell'app dietro all'overlay. Quando è aperto
  // un overlay (conferma/esito) il trap si restringe AL SOLO overlay, altrimenti
  // il Tab raggiungerebbe i controlli del form sottostanti, nascosti dietro di
  // esso ma ancora nel DOM.
  const onPanelKeyDown = (e: React.KeyboardEvent<HTMLDivElement>): void => {
    if (e.key !== 'Tab') return;
    const root = overlayActive ? overlayRef.current : panelRef.current;
    if (!root) return;
    const focusable = root.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  // All'apertura di un overlay sposta il focus al suo interno: così il trap parte
  // già confinato e l'Invio non attiva per sbaglio un bottone sottostante.
  useEffect(() => {
    if (!overlayActive) return;
    const root = overlayRef.current;
    if (!root) return;
    const target = root.querySelector<HTMLElement>(
      'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    (target || root).focus?.();
  }, [overlayActive]);

  // Incolla (Cmd/Ctrl+V) di uno screenshot direttamente nel messaggio.
  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>): void => {
    const imgs = Array.from(e.clipboardData?.items || [])
      .filter((it) => it.type?.startsWith('image/'))
      .map((it) => it.getAsFile())
      .filter((f): f is File => !!f);
    if (imgs.length) {
      e.preventDefault();
      addFiles(imgs);
    }
  };

  // Chiusura "sicura": durante l'invio è bloccata; se ci sono modifiche in
  // sospeso (e non è già stato inviato) chiede conferma.
  const requestClose = (): void => {
    if (busy) return;
    if (isDirty && status !== 'sent') setConfirm('close');
    else onClose();
  };

  // Esc: annulla prima l'eventuale conferma aperta, altrimenti tenta la chiusura.
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      if (confirm) setConfirm(null);
      else requestClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [confirm, isDirty, status]);

  const doSend = async (): Promise<void> => {
    setConfirm(null);
    setStatus('sending');
    setError(null);
    try {
      const res = await window.electronAPI?.sendFeedback?.(
        message.trim(),
        attachments.map((a) => ({ filename: a.name, content: a.base64, type: a.type })),
      );
      if (res?.ok) {
        // L'auto-chiusura (dopo un breve riscontro visivo) è gestita
        // dall'effect su status === 'sent', con cleanup del timer.
        setStatus('sent');
      } else {
        setStatus('error');
        setError(res?.error || t('sendFailed'));
      }
    } catch (err) {
      setStatus('error');
      setError(err instanceof Error ? err.message : t('sendFailed'));
    }
  };

  return (
    <div
      data-testid="feedback-modal"
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-6 u-backdrop-in"
      // Click sullo sfondo: volutamente nessuna chiusura.
      onClick={(e) => e.stopPropagation()}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="feedback-title"
        ref={panelRef}
        onKeyDown={onPanelKeyDown}
        className="relative bg-[#1a1a1a] border border-[#2e2e2e] rounded-xl shadow-2xl w-full max-w-md overflow-hidden u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div
          {...inertProp(overlayActive)}
          className="flex items-center justify-between px-5 h-12 border-b border-[#2e2e2e]"
        >
          <span
            id="feedback-title"
            className="flex items-center gap-2 text-white text-sm font-semibold font-display"
          >
            <MessageSquare size={16} className="text-[#7B5CFF]" />
            {t('title')}
          </span>
          <button
            onClick={requestClose}
            disabled={busy}
            title={tc('close')}
            className="flex items-center justify-center w-8 h-8 -mr-2 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40"
          >
            <X size={16} />
          </button>
        </div>

        <div {...inertProp(overlayActive)} className="px-5 py-5 space-y-4">
          <p className="text-[#888] text-sm leading-relaxed">{t('intro')}</p>

          <div>
            <div className="flex items-center justify-between mb-1.5">
              <label htmlFor="feedback-message" className="block text-xs font-medium text-gray-400">
                {t('messageLabel')}
              </label>
              <span
                className={`text-[11px] tabular-nums ${
                  message.length >= MAX_MESSAGE_LEN ? 'text-amber-500' : 'text-gray-600'
                }`}
              >
                {message.length}/{MAX_MESSAGE_LEN}
              </span>
            </div>
            <textarea
              id="feedback-message"
              ref={textareaRef}
              data-testid="feedback-message"
              value={message}
              onChange={(e) => setMessage(e.target.value.slice(0, MAX_MESSAGE_LEN))}
              onPaste={onPaste}
              disabled={busy}
              rows={5}
              maxLength={MAX_MESSAGE_LEN}
              placeholder={t('messagePlaceholder')}
              className="w-full bg-[#0f0f0f] border border-[#2e2e2e] rounded-md px-3 py-2 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] transition-colors resize-none disabled:opacity-50"
            />
          </div>

          <div>
            <div className="flex items-center justify-between mb-1.5">
              <label className="block text-xs font-medium text-gray-400">
                {t('screenshotLabel')} <span className="text-gray-600">{t('optional')}</span>
              </label>
              <button
                type="button"
                data-testid="feedback-attach"
                onClick={() => fileInputRef.current?.click()}
                disabled={busy || attachments.length >= MAX_FILES}
                className="flex items-center gap-1.5 px-2 py-1 rounded-md text-xs text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40 disabled:cursor-not-allowed"
              >
                <Paperclip size={13} />
                {t('attach')}
              </button>
            </div>
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              multiple
              className="hidden"
              onChange={(e) => {
                addFiles(e.target.files);
                e.target.value = '';
              }}
            />

            {attachments.length === 0 ? (
              <p className="text-[11px] text-gray-600">{t('attachHint')}</p>
            ) : (
              <div className="flex flex-wrap gap-2">
                {attachments.map((a) => (
                  <div
                    key={a.id}
                    className="group relative w-16 h-16 rounded-md overflow-hidden border border-[#2e2e2e] bg-[#0f0f0f]"
                  >
                    <img src={a.dataUrl} alt={a.name} className="w-full h-full object-cover" />
                    {!busy && (
                      <button
                        type="button"
                        onClick={() => removeAttachment(a.id)}
                        title={tc('remove')}
                        className="absolute top-0.5 right-0.5 flex items-center justify-center w-5 h-5 rounded bg-black/70 text-white opacity-0 group-hover:opacity-100 hover:bg-black transition-opacity u-press"
                      >
                        <X size={12} />
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}

            {attachWarn && (
              <p className="mt-1.5 text-[11px] text-amber-500 whitespace-pre-line">{attachWarn}</p>
            )}
          </div>

          {status === 'error' && (
            <div
              role="alert"
              aria-live="assertive"
              className="flex items-start gap-2 text-sm text-red-400 u-fade-in"
            >
              <AlertCircle size={16} className="shrink-0 mt-0.5" />
              <span>{error}</span>
            </div>
          )}
        </div>

        <div
          {...inertProp(overlayActive)}
          className="flex justify-end gap-2 px-5 py-3 border-t border-[#2e2e2e]"
        >
          <button
            onClick={requestClose}
            disabled={busy}
            className="px-3.5 py-1.5 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#2a2a2a] transition-colors u-press disabled:opacity-40"
          >
            {tc('cancel')}
          </button>
          <button
            data-testid="feedback-send"
            onClick={() => setConfirm('send')}
            disabled={!canSend}
            className="flex items-center gap-2 px-4 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-40 disabled:cursor-not-allowed transition-[background-color,opacity] duration-200 u-press"
          >
            {busy && <Loader size={14} className="animate-spin" />}
            {status === 'error' ? tc('retry') : t('send')}
          </button>
        </div>

        {confirm && (
          <div
            ref={overlayRef}
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="feedback-confirm-text"
            className="absolute inset-0 bg-[#1a1a1a]/95 flex items-center justify-center p-6 u-fade-in"
          >
            <div
              id="feedback-confirm-text"
              className="w-full max-w-xs text-center space-y-4 u-pop-in"
            >
              <AlertCircle
                size={32}
                className={`mx-auto ${confirm === 'send' ? 'text-[#7B5CFF]' : 'text-amber-500'}`}
              />
              {confirm === 'send' ? (
                <p className="text-[#ccc] text-sm leading-relaxed">
                  {t('confirmSend')}
                  <br />
                  {t('confirmSendDetail')}
                </p>
              ) : (
                <p className="text-[#ccc] text-sm leading-relaxed">{t('confirmClose')}</p>
              )}
              <div className="flex flex-col gap-2">
                {confirm === 'send' ? (
                  <>
                    <button
                      data-testid="feedback-confirm-send"
                      onClick={doSend}
                      className="w-full px-4 py-2 rounded-lg bg-[#7B5CFF] text-white text-sm font-medium hover:bg-[#5A3DDE] transition-colors u-press"
                    >
                      {t('yesSend')}
                    </button>
                    <button
                      onClick={() => setConfirm(null)}
                      className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                    >
                      {tc('cancel')}
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      data-testid="feedback-confirm-close"
                      onClick={onClose}
                      className="w-full px-4 py-2 rounded-lg bg-white text-black text-sm font-medium hover:bg-[#e0e0e0] transition-colors u-press"
                    >
                      {t('closeWithoutSending')}
                    </button>
                    <button
                      onClick={() => setConfirm(null)}
                      className="w-full px-4 py-2 rounded-lg text-[#888] hover:text-white transition-colors text-sm u-press"
                    >
                      {t('keepWriting')}
                    </button>
                  </>
                )}
              </div>
            </div>
          </div>
        )}

        {status === 'sent' && (
          <div
            ref={overlayRef}
            role="alertdialog"
            aria-modal="true"
            aria-live="assertive"
            tabIndex={-1}
            className="absolute inset-0 bg-[#1a1a1a]/95 flex items-center justify-center p-6 u-fade-in outline-none"
          >
            <div className="text-center space-y-3 u-pop-in">
              <CheckCircle2 size={40} className="mx-auto text-emerald-500" />
              <p className="text-[#ccc] text-sm">{t('sent')}</p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
