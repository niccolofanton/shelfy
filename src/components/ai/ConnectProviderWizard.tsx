import React, { useId, useState } from 'react';
import { createPortal } from 'react-dom';
import {
  AI_TASKS,
  type AiProviderInput,
  type AiProvidersApi,
  type AiProviderSummary,
  type AiProviderTest,
  type AiTask,
} from '../../api/aiProviders';
import { useDialog } from '../../hooks/useDialog';
import { useFailureText } from '../../hooks/useFailureText';
import { useT } from '../../i18n';
import { InlineNote, INPUT, PRIMARY_BUTTON } from '../../views/settings/ui';
import { notifyProviderSettingsChanged } from './providerConnection';

const STEPS = ['preset', 'key', 'test', 'consent', 'route'] as const;
type Step = (typeof STEPS)[number];
const PRESETS = {
  custom: { kind: 'openai_compatible', label: '', baseUrl: '' },
  openai: { kind: 'openai_compatible', label: 'OpenAI', baseUrl: 'https://api.openai.com/v1' },
  anthropic: { kind: 'anthropic', label: 'Anthropic', baseUrl: 'https://api.anthropic.com' },
} as const;
function modelForTask(
  models: Partial<Record<AiTask, string | null>>,
  task: AiTask,
): string | null | undefined {
  if (task === 'qc') return models.qc || models.catalog;
  if (task === 'cluster' || task === 'alias') return models[task] || models.chat || models.suggest;
  return models[task];
}
export function providerSupports(provider: AiProviderSummary, task: AiTask): boolean {
  if (provider.kind === 'anthropic' && (task === 'embed' || task === 'stt')) return false;
  if (provider.taskModels) return !!modelForTask(provider.taskModels, task);
  return task === 'catalog' || task === 'qc'
    ? !!provider.models.vision
    : task === 'embed'
      ? !!provider.models.embed
      : task === 'stt'
        ? provider.stt
        : !!provider.models.text;
}

export default function ConnectProviderWizard({
  api,
  task,
  provider,
  onClose,
}: {
  api: AiProvidersApi;
  task?: AiTask;
  provider?: AiProviderSummary;
  onClose: () => void;
}): React.JSX.Element {
  const t = useT('aiProviders');
  const failure = useFailureText();
  const titleId = useId();
  const [step, setStep] = useState<Step>(provider ? 'key' : 'preset');
  const [preset, setPreset] = useState<keyof typeof PRESETS>('custom');
  const [id] = useState(() => provider?.id ?? `byok_${crypto.randomUUID().replaceAll('-', '')}`);
  const [kind, setKind] = useState<AiProviderInput['kind']>(
    provider?.kind === 'anthropic' ? 'anthropic' : 'openai_compatible',
  );
  const [label, setLabel] = useState(provider?.label ?? '');
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? '');
  const [models, setModels] = useState<Partial<Record<AiTask, string>>>(() => {
    if (provider?.taskModels)
      return Object.fromEntries(
        Object.entries(provider.taskModels).filter(([, value]) => typeof value === 'string'),
      );
    return provider
      ? {
          chat: provider.models.text ?? '',
          catalog: provider.models.vision ?? '',
          embed: provider.models.embed ?? '',
        }
      : {};
  });
  const [key, setKey] = useState('');
  const [inputPrice, setInputPrice] = useState(
    provider?.prices?.inputPerMillionUsd.toString() ?? '',
  );
  const [outputPrice, setOutputPrice] = useState(
    provider?.prices?.outputPerMillionUsd.toString() ?? '',
  );
  const [saved, setSaved] = useState(provider);
  const [result, setResult] = useState<AiProviderTest | undefined>(provider?.test ?? undefined);
  const [accepted, setAccepted] = useState(false);
  const [routeTask, setRouteTask] = useState<AiTask>(task ?? 'chat');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const dialogRef = useDialog<HTMLDivElement>({
    onClose: () => {
      if (!busy) onClose();
    },
  });
  const manager = api.management;
  const refresh = async (): Promise<void> => {
    setSaved((await api.list()).find((p) => p.id === id));
  };
  const operation = async (run: () => Promise<void>): Promise<void> => {
    if (busy || !manager) return;
    setBusy(true);
    setError(null);
    setValidation(null);
    try {
      await run();
    } catch (cause) {
      setError(cause);
    } finally {
      setBusy(false);
    }
  };
  const save = async (): Promise<void> => {
    if (!label.trim() || !baseUrl.trim() || (!saved?.configured && !key)) {
      setValidation(t('requiredConfiguration'));
      return;
    }
    const selected = Object.fromEntries(
      Object.entries(models)
        .filter(([, value]) => value.trim())
        .map(([name, value]) => [name, value.trim()]),
    );
    if (!Object.keys(selected).length) {
      setValidation(t('modelsRequired'));
      return;
    }
    if (
      task &&
      (!modelForTask(selected, task) ||
        (kind === 'anthropic' && (task === 'embed' || task === 'stt')))
    ) {
      setValidation(t('modelForTaskRequired', { task: t(`task.${task}`) }));
      return;
    }
    let prices: AiProviderInput['prices'];
    if (inputPrice !== '' || outputPrice !== '') {
      if (
        inputPrice === '' ||
        outputPrice === '' ||
        ![Number(inputPrice), Number(outputPrice)].every(
          (price) => Number.isFinite(price) && price >= 0,
        )
      ) {
        setValidation(t('pricesRequired'));
        return;
      }
      prices = { inputPerMillionUsd: Number(inputPrice), outputPerMillionUsd: Number(outputPrice) };
    }
    const input: AiProviderInput = {
      kind,
      label: label.trim(),
      baseUrl: baseUrl.trim(),
      models: selected,
      ...(prices ? { prices } : {}),
      ...(key ? { key } : {}),
    };
    // Erase the controlled input before any request/re-authentication can wait.
    setKey('');
    await operation(async () => {
      await manager!.save(id, input);
      await refresh();
      setResult(undefined);
      setAccepted(false);
      setStep('test');
      notifyProviderSettingsChanged({ providerId: id });
    });
  };
  const tested =
    !!result &&
    [result.models, result.text, result.vision, result.schema].every(
      (check) => check.ok || check.skipped,
    );
  const close = (): void => {
    if (!busy) {
      setKey('');
      onClose();
    }
  };
  return createPortal(
    <div
      className="fixed inset-0 z-[80] flex items-center justify-center bg-black/70 p-4"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <section
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className="max-h-[90vh] w-full max-w-xl overflow-y-auto rounded-xl border border-[#303030] bg-[#161616] p-5 text-gray-200"
      >
        <div className="flex items-center justify-between gap-3">
          <h2 id={titleId} className="font-semibold">
            {t(provider ? 'editProvider' : 'connectProvider')}
          </h2>
          <button type="button" disabled={busy} onClick={close} aria-label={t('close')}>
            ×
          </button>
        </div>
        <ol
          aria-label={t('connectionSteps')}
          className="my-4 flex flex-wrap gap-3 text-xs text-gray-400"
        >
          {STEPS.map((value, index) => (
            <li
              key={value}
              aria-current={step === value ? 'step' : undefined}
              className={step === value ? 'font-semibold text-violet-300' : ''}
            >
              {index + 1}. {t(`step.${value}`)}
            </li>
          ))}
        </ol>
        {task && <p className="mb-4 text-sm">{t('connectForTask', { task: t(`task.${task}`) })}</p>}
        <fieldset disabled={busy} className="space-y-4">
          {(step === 'preset' || step === 'key') && (
            <>
              {step === 'preset' && (
                <label className="block text-xs">
                  {t('preset')}
                  <select
                    className={`${INPUT} mt-1 w-full`}
                    value={preset}
                    onChange={(event) => {
                      const name = event.target.value as keyof typeof PRESETS;
                      setPreset(name);
                      const value = PRESETS[name];
                      setKind(value.kind);
                      setLabel(value.label);
                      setBaseUrl(value.baseUrl);
                    }}
                  >
                    <option value="custom">{t('customProvider')}</option>
                    <option value="openai">OpenAI</option>
                    <option value="anthropic">Anthropic</option>
                  </select>
                </label>
              )}
              <label className="block text-xs">
                {t('providerLabel')}
                <input
                  className={`${INPUT} mt-1 w-full`}
                  value={label}
                  maxLength={128}
                  onChange={(event) => setLabel(event.target.value)}
                />
              </label>
              <label className="block text-xs">
                {t('baseUrl')}
                <input
                  type="url"
                  className={`${INPUT} mt-1 w-full`}
                  value={baseUrl}
                  onChange={(event) => setBaseUrl(event.target.value)}
                />
              </label>
              <p className="text-xs text-gray-500">{t('publicUrlRequired')}</p>
              <label className="block text-xs">
                {t('protocol')}
                <select
                  className={`${INPUT} mt-1 w-full`}
                  value={kind}
                  onChange={(event) => setKind(event.target.value as AiProviderInput['kind'])}
                >
                  <option value="openai_compatible">OpenAI-compatible</option>
                  <option value="anthropic">Anthropic</option>
                </select>
              </label>
              {step === 'preset' && (
                <button
                  type="button"
                  className={PRIMARY_BUTTON}
                  disabled={!label.trim() || !baseUrl.trim()}
                  onClick={() => {
                    setError(null);
                    setStep('key');
                  }}
                >
                  {t('continue')}
                </button>
              )}
              {step === 'key' && (
                <>
                  <label className="block text-xs">
                    {t('apiKey')}
                    <input
                      type="password"
                      autoComplete="new-password"
                      spellCheck={false}
                      className={`${INPUT} mt-1 w-full`}
                      value={key}
                      onChange={(event) => setKey(event.target.value)}
                    />
                  </label>
                  <p className="text-xs text-gray-500">
                    {t(saved?.configured ? 'keyReplacement' : 'keyWriteOnly')}
                    {saved?.last4 ? ` ••••${saved.last4}` : ''}
                  </p>
                  <p className="text-xs text-gray-500">{t('typedModels')}</p>
                  <div className="grid grid-cols-2 gap-3">
                    {AI_TASKS.map((value) => (
                      <label key={value} className="text-xs">
                        {t(`task.${value}`)}
                        <input
                          className={`${INPUT} mt-1 w-full`}
                          value={models[value] ?? ''}
                          onChange={(event) =>
                            setModels((previous) => ({ ...previous, [value]: event.target.value }))
                          }
                        />
                      </label>
                    ))}
                  </div>
                  <div className="grid grid-cols-2 gap-3">
                    <label className="text-xs">
                      {t('inputPrice')}
                      <input
                        type="number"
                        min={0}
                        step="any"
                        className={`${INPUT} mt-1 w-full`}
                        value={inputPrice}
                        onChange={(event) => setInputPrice(event.target.value)}
                      />
                    </label>
                    <label className="text-xs">
                      {t('outputPrice')}
                      <input
                        type="number"
                        min={0}
                        step="any"
                        className={`${INPUT} mt-1 w-full`}
                        value={outputPrice}
                        onChange={(event) => setOutputPrice(event.target.value)}
                      />
                    </label>
                  </div>
                  <p className="text-xs text-gray-500">{t('pricesOptional')}</p>
                  <button type="button" className={PRIMARY_BUTTON} onClick={() => void save()}>
                    {t(busy ? 'saving' : 'saveProvider')}
                  </button>
                </>
              )}
            </>
          )}
          {step === 'test' && (
            <>
              <p className="text-sm">{t('syntheticOnly')}</p>
              {saved?.last4 && <p className="text-xs">{t('storedKey', { last4: saved.last4 })}</p>}
              {result && (
                <dl className="grid grid-cols-2 gap-3 text-sm">
                  {(['models', 'text', 'vision', 'schema'] as const).map((name) => (
                    <React.Fragment key={name}>
                      <dt>{t(`probe.${name}`)}</dt>
                      <dd
                        className={
                          result[name].ok
                            ? 'text-emerald-400'
                            : result[name].skipped
                              ? 'text-gray-500'
                              : 'text-amber-300'
                        }
                      >
                        {t(
                          result[name].ok
                            ? 'probePassed'
                            : result[name].skipped
                              ? 'probeSkipped'
                              : 'probeFailed',
                        )}
                        {result[name].error ? ` (${t(`probeError.${result[name].error}`)})` : ''}
                      </dd>
                    </React.Fragment>
                  ))}
                </dl>
              )}
              <div className="flex flex-wrap gap-3">
                <button
                  type="button"
                  className={PRIMARY_BUTTON}
                  onClick={() =>
                    void operation(async () => {
                      setResult(await manager!.test(id));
                      await refresh();
                      notifyProviderSettingsChanged({ providerId: id });
                    })
                  }
                >
                  {t(busy ? 'testing' : 'runTest')}
                </button>
                <button
                  type="button"
                  disabled={!tested}
                  className={PRIMARY_BUTTON}
                  onClick={() => setStep('consent')}
                >
                  {t('continue')}
                </button>
                <button type="button" onClick={() => setStep('key')}>
                  {t('editProvider')}
                </button>
              </div>
            </>
          )}
          {step === 'consent' && (
            <>
              <p className="text-sm">
                {t('providerConsent', {
                  label: saved?.label ?? label,
                  baseUrl: saved?.baseUrl ?? baseUrl,
                })}
              </p>
              <label className="flex items-start gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={accepted}
                  onChange={(event) => setAccepted(event.target.checked)}
                />
                <span>{t('acceptProviderConsent')}</span>
              </label>
              <button
                type="button"
                className={PRIMARY_BUTTON}
                disabled={!accepted}
                onClick={() =>
                  void operation(async () => {
                    await manager!.consent(id, saved?.consentVersion ?? 'ai-provider-v1');
                    await refresh();
                    notifyProviderSettingsChanged({ providerId: id });
                    setStep('route');
                  })
                }
              >
                {t('recordConsent')}
              </button>
            </>
          )}
          {step === 'route' && (
            <>
              <label className="block text-xs">
                {t('routeTask')}
                <select
                  className={`${INPUT} mt-1 w-full`}
                  value={routeTask}
                  onChange={(event) => setRouteTask(event.target.value as AiTask)}
                >
                  {AI_TASKS.map((value) => (
                    <option
                      value={value}
                      key={value}
                      disabled={!saved || !providerSupports(saved, value)}
                    >
                      {t(`task.${value}`)}
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className={PRIMARY_BUTTON}
                disabled={!saved || !providerSupports(saved, routeTask)}
                onClick={() =>
                  void operation(async () => {
                    const settings = await api.getSettings();
                    await api.updateSettings({
                      ...settings,
                      aiRouting: { ...settings.aiRouting, [routeTask]: id },
                    });
                    notifyProviderSettingsChanged({ task: routeTask, providerId: id });
                    onClose();
                  })
                }
              >
                {t('activateRoute')}
              </button>
            </>
          )}
        </fieldset>
        {validation && <InlineNote tone="error">{validation}</InlineNote>}
        {error != null && <InlineNote tone="error">{failure(error)}</InlineNote>}
      </section>
    </div>,
    document.body,
  );
}
