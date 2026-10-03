import React, { useEffect, useState } from 'react';
import { Activity, Cpu, GitBranch, SlidersHorizontal } from 'lucide-react';
import {
  AI_TASKS,
  type AiProvidersApi,
  type AiProviderSettings,
  type AiUsageDay,
} from '../../api/aiProviders';
import { useAiProviders } from '../../components/ai/useAiProviders';
import { providerSupports } from '../../components/ai/ConnectProviderWizard';
import {
  requestProviderConnection,
  notifyProviderSettingsChanged,
  PROVIDER_SETTINGS_CHANGED,
  type ProviderSettingsChange,
} from '../../components/ai/providerConnection';
import { useFailureText } from '../../hooks/useFailureText';
import { localeTag, useLang, useT } from '../../i18n';
import { Card, CardHeader, InlineNote, INPUT, Loading, PRIMARY_BUTTON } from './ui';

const TOGGLES = [
  'aiSuggestions',
  'aiVisionQc',
  'aiAutoAnalyzeWebsites',
  'aiDictationInterim',
] as const;

export default function AiSettings({ api }: { api: AiProvidersApi }): React.JSX.Element {
  const t = useT('aiProviders');
  const failure = useFailureText();
  const { lang } = useLang();
  const { providers, error: providerError } = useAiProviders(api);
  const [draft, setDraft] = useState<AiProviderSettings | null>(null);
  const [saved, setSaved] = useState<AiProviderSettings | null>(null);
  const [usage, setUsage] = useState<AiUsageDay[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [usageError, setUsageError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [success, setSuccess] = useState(false);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [manageError, setManageError] = useState<unknown>(null);
  useEffect(() => {
    let active = true;
    void api
      .getSettings()
      .then((value) => {
        if (active) {
          setDraft(value);
          setSaved(value);
        }
      })
      .catch((error) => {
        if (active) setLoadError(error);
      });
    void api
      .usage(30)
      .then((value) => {
        if (active) setUsage(value);
      })
      .catch((error) => {
        if (active) setUsageError(error);
      });
    return () => {
      active = false;
    };
  }, [api]);
  useEffect(() => {
    let active = true;
    const changed = (event: Event): void => {
      const change = (event as CustomEvent<ProviderSettingsChange>).detail;
      if (change.aiSuggestions !== undefined) return;
      void api
        .getSettings()
        .then((value) => {
          if (!active) return;
          setSaved(value);
          setDraft((previous) => {
            if (!previous) return value;
            const aiRouting = { ...previous.aiRouting };
            if (change.task && change.providerId) aiRouting[change.task] = change.providerId;
            if (change.deletedId)
              for (const task of AI_TASKS)
                if (aiRouting[task] === change.deletedId) delete aiRouting[task];
            return { ...previous, aiRouting };
          });
        })
        .catch((cause) => {
          if (active) setManageError(cause);
        });
    };
    window.addEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    return () => {
      active = false;
      window.removeEventListener(PROVIDER_SETTINGS_CHANGED, changed);
    };
  }, [api]);
  const remove = async (id: string): Promise<void> => {
    if (!api.management || deleting) return;
    setDeleting(id);
    setManageError(null);
    try {
      await api.management.delete(id);
      notifyProviderSettingsChanged({ deletedId: id });
    } catch (cause) {
      setManageError(cause);
    } finally {
      setDeleting(null);
    }
  };
  const change = (patch: Partial<AiProviderSettings>): void => {
    setDraft((previous) => previous && { ...previous, ...patch });
    setSuccess(false);
    setSaveError(null);
  };
  const save = async (): Promise<void> => {
    if (!draft || busy) return;
    setBusy(true);
    setSuccess(false);
    setSaveError(null);
    try {
      const value = await api.updateSettings(draft);
      setDraft(value);
      setSaved(value);
      setSuccess(true);
      notifyProviderSettingsChanged({ aiSuggestions: value.aiSuggestions });
    } catch (error) {
      setSaveError(error);
    } finally {
      setBusy(false);
    }
  };
  const number = new Intl.NumberFormat(localeTag(lang));
  const money = new Intl.NumberFormat(localeTag(lang), {
    style: 'currency',
    currency: 'USD',
    maximumFractionDigits: 4,
  });
  return (
    <div className="space-y-4" data-testid="ai-provider-settings">
      <Card testId="ai-providers">
        <CardHeader
          icon={Cpu}
          title={t('providersTitle')}
          description={t('providersDesc')}
          aside={
            api.management && (
              <button
                type="button"
                className={PRIMARY_BUTTON}
                onClick={() => requestProviderConnection()}
              >
                {t('addProvider')}
              </button>
            )
          }
        />
        {providers === null && providerError === null && <Loading />}
        {providerError != null && <InlineNote tone="error">{failure(providerError)}</InlineNote>}
        {manageError != null && <InlineNote tone="error">{failure(manageError)}</InlineNote>}
        {providers?.length === 0 && <InlineNote tone="info">{t('noProviders')}</InlineNote>}
        <div className="mt-4 space-y-3">
          {providers?.map((provider) => (
            <div
              key={provider.id}
              data-testid={`provider-${provider.id}`}
              className="rounded-lg border border-[#303030] p-3 text-xs"
            >
              <div className="flex items-center justify-between gap-2">
                <strong className="text-gray-100">{provider.label}</strong>
                <span className={provider.status === 'ok' ? 'text-emerald-400' : 'text-amber-300'}>
                  {t(`status.${provider.status}`)}
                </span>
              </div>
              <p className="mt-1 text-gray-400">{t(provider.managed ? 'managed' : 'byok')}</p>
              <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-gray-400">
                {provider.taskModels
                  ? AI_TASKS.map((task) => (
                      <React.Fragment key={task}>
                        <dt>{t(`task.${task}`)}</dt>
                        <dd className="break-all text-gray-200">
                          {provider.taskModels?.[task] ?? t('notConfigured')}
                        </dd>
                      </React.Fragment>
                    ))
                  : (['text', 'vision', 'embed'] as const).map((kind) => (
                      <React.Fragment key={kind}>
                        <dt>{t(`model.${kind}`)}</dt>
                        <dd className="break-all text-gray-200">
                          {provider.models[kind] ?? t('notConfigured')}
                        </dd>
                      </React.Fragment>
                    ))}
                {!provider.taskModels && (
                  <>
                    <dt>{t('model.stt')}</dt>
                    <dd>{t(provider.stt ? 'available' : 'notConfigured')}</dd>
                  </>
                )}
              </dl>
              {!provider.managed && (
                <>
                  {provider.last4 && (
                    <p className="mt-2 text-gray-400">
                      {t('storedKey', { last4: provider.last4 })}
                    </p>
                  )}
                  {provider.baseUrl && (
                    <p className="mt-1 break-all text-gray-500">{provider.baseUrl}</p>
                  )}
                  <p className="mt-2 text-gray-400">
                    {t(provider.consent ? 'consentRecorded' : 'consentNeeded')}
                  </p>
                  {provider.prices && (
                    <p className="mt-1 text-gray-400">
                      {t('providerPrices', {
                        input: number.format(provider.prices.inputPerMillionUsd),
                        output: number.format(provider.prices.outputPerMillionUsd),
                      })}
                    </p>
                  )}
                  {provider.test && (
                    <p className="mt-1 text-gray-400">
                      {t('lastTest')}:{' '}
                      {(['models', 'text', 'vision', 'schema'] as const)
                        .map(
                          (name) =>
                            `${t(`probe.${name}`)}: ${t(provider.test![name].ok ? 'probePassed' : provider.test![name].skipped ? 'probeSkipped' : 'probeFailed')}`,
                        )
                        .join(' · ')}
                    </p>
                  )}
                  {api.management && (
                    <div className="mt-3 flex gap-4">
                      <button
                        type="button"
                        disabled={deleting != null}
                        onClick={() => requestProviderConnection(undefined, provider)}
                      >
                        {t('editProvider')}
                      </button>
                      <button
                        type="button"
                        className="text-red-300"
                        disabled={deleting != null}
                        onClick={() => void remove(provider.id)}
                      >
                        {t(deleting === provider.id ? 'deletingProvider' : 'deleteProvider')}
                      </button>
                    </div>
                  )}
                </>
              )}
            </div>
          ))}
        </div>
      </Card>
      {loadError != null && <InlineNote tone="error">{failure(loadError)}</InlineNote>}
      {!draft && loadError === null && <Loading />}
      {draft && (
        <fieldset disabled={busy} className="space-y-4">
          <Card testId="ai-routing">
            <CardHeader icon={GitBranch} title={t('routingTitle')} description={t('routingDesc')} />
            <div className="mt-4 space-y-3">
              {AI_TASKS.map((task) => {
                const chosen = draft.aiRouting[task];
                const vision = task === 'catalog' || task === 'qc';
                const supports = (provider: NonNullable<typeof providers>[number]): boolean =>
                  providerSupports(provider, task);
                const effective = chosen
                  ? providers?.find((p) => p.id === chosen)
                  : providers?.find(supports);
                const noRoute = providers !== null && (!effective || !supports(effective));
                return (
                  <div key={task}>
                    <label className="grid grid-cols-[minmax(100px,1fr)_minmax(120px,1fr)] items-center gap-3 text-xs text-gray-300">
                      <span>{t(`task.${task}`)}</span>
                      <select
                        className={INPUT}
                        aria-label={t(`task.${task}`)}
                        value={chosen ?? ''}
                        onChange={(event) => {
                          const aiRouting = { ...draft.aiRouting };
                          if (event.target.value) aiRouting[task] = event.target.value;
                          else delete aiRouting[task];
                          change({ aiRouting });
                        }}
                      >
                        <option value="">{t('defaultRoute')}</option>
                        {chosen && !providers?.some((p) => p.id === chosen) && (
                          <option value={chosen}>{t('missingProvider', { id: chosen })}</option>
                        )}
                        {providers?.map((p) => (
                          <option key={p.id} value={p.id}>
                            {p.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    {noRoute && (
                      <InlineNote tone="info">
                        {t(vision ? 'visionRequired' : 'noRoute')}
                        {api.management && (
                          <button
                            type="button"
                            className="ml-2 underline"
                            onClick={() => requestProviderConnection(task)}
                          >
                            {t('connectProvider')}
                          </button>
                        )}
                      </InlineNote>
                    )}
                  </div>
                );
              })}
            </div>
          </Card>
          <Card testId="ai-preferences">
            <CardHeader icon={SlidersHorizontal} title={t('preferencesTitle')} />
            <label className="mt-4 flex items-center justify-between gap-4 text-xs text-gray-300">
              <span>{t('concurrency')}</span>
              <select
                aria-label={t('concurrency')}
                className={`${INPUT} max-w-20`}
                value={draft.aiConcurrency}
                onChange={(event) => change({ aiConcurrency: Number(event.target.value) })}
              >
                {Array.from({ length: 8 }, (_, i) => (
                  <option key={i} value={i + 1}>
                    {i + 1}
                  </option>
                ))}
              </select>
            </label>
            <p className="mt-1 text-xs text-gray-500">{t('concurrencyDesc')}</p>
            <div className="mt-4 space-y-3">
              {TOGGLES.map((key) => (
                <label key={key} className="flex items-center gap-3 text-xs text-gray-300">
                  <input
                    type="checkbox"
                    checked={draft[key]}
                    onChange={(event) => change({ [key]: event.target.checked })}
                    className="accent-[#7B5CFF]"
                  />
                  <span>{t(key)}</span>
                </label>
              ))}
            </div>
            <button
              type="button"
              className={`${PRIMARY_BUTTON} mt-5`}
              disabled={busy || JSON.stringify(saved) === JSON.stringify(draft)}
              onClick={() => void save()}
            >
              {t(busy ? 'saving' : 'save')}
            </button>
            {success && <InlineNote tone="ok">{t('saved')}</InlineNote>}
            {saveError != null && <InlineNote tone="error">{failure(saveError)}</InlineNote>}
          </Card>
        </fieldset>
      )}
      <Card testId="ai-usage">
        <CardHeader icon={Activity} title={t('usageTitle')} description={t('usageDesc')} />
        {usage === null && usageError === null && <Loading />}
        {usageError != null && <InlineNote tone="error">{failure(usageError)}</InlineNote>}
        {usage?.length === 0 && <InlineNote tone="info">{t('noUsage')}</InlineNote>}
        {!!usage?.length && (
          <div className="mt-4 overflow-x-auto">
            <table className="w-full text-left text-xs text-gray-300">
              <thead className="text-gray-500">
                <tr>
                  {['day', 'calls', 'inputTokens', 'outputTokens', 'cost'].map((key) => (
                    <th key={key} className="py-2 pr-3 font-normal">
                      {t(key)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {usage.map((row) => (
                  <tr key={row.day} className="border-t border-[#282828]">
                    <td className="py-2 pr-3 whitespace-nowrap">{row.day}</td>
                    <td>{number.format(row.calls)}</td>
                    <td>{number.format(row.inputTokens)}</td>
                    <td>{number.format(row.outputTokens)}</td>
                    <td>{row.cost == null ? '—' : money.format(row.cost)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <p className="mt-2 text-xs text-gray-500">{t('unknownCost')}</p>
          </div>
        )}
      </Card>
    </div>
  );
}
