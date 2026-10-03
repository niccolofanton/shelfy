import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  AiProvidersApi,
  AiProviderSettings,
  AiProviderSummary,
  ProviderState,
} from '../../src/api/aiProviders';
import { I18nProvider } from '../../src/i18n';
import AiSettings from '../../src/views/settings/Ai';
import ProviderStatusBanner from '../../src/components/ai/ProviderStatusBanner';
import { createAiProvidersApi } from '../src/api/aiProviders';
import { createHttpClient } from '../src/api/httpClient';
import type { Http } from '../src/api/http';
import type { EventStream } from '../src/api/events';
import { OWNER } from './authFakes';

const DEFAULTS: AiProviderSettings = {
  aiRouting: {},
  aiConcurrency: 4,
  aiSuggestions: true,
  aiVisionQc: false,
  aiAutoAnalyzeWebsites: false,
  aiDictationInterim: false,
};
const NODE: AiProviderSummary = {
  id: 'operator',
  kind: 'operator',
  label: 'My AI node',
  managed: true,
  models: { text: 'text-model', vision: 'vision-model', embed: null },
  stt: false,
  status: 'ok',
};
function fixture(providers = [NODE]) {
  const status = new Set<(value: { providerId: string; state: ProviderState }) => void>();
  const resync = new Set<() => void>();
  const api: AiProvidersApi = {
    list: vi.fn(async () => providers),
    getSettings: vi.fn(async () => structuredClone(DEFAULTS)),
    updateSettings: vi.fn(async (settings) => settings),
    usage: vi.fn(async () => [
      { day: '2026-10-03', calls: 2, inputTokens: 120, outputTokens: 40, cost: null },
      { day: '2026-10-02', calls: 1, inputTokens: 20, outputTokens: 30, cost: 0.03 },
    ]),
    onStatus(listener) {
      status.add(listener);
      return () => status.delete(listener);
    },
    onResync(listener) {
      resync.add(listener);
      return () => resync.delete(listener);
    },
  };
  return {
    api,
    emit(state: ProviderState) {
      for (const listener of status) listener({ providerId: 'operator', state });
    },
    resync() {
      for (const listener of resync) listener();
    },
    listeners: status,
  };
}
function settings(api: AiProvidersApi, lang = 'en') {
  localStorage.setItem('app:language', lang);
  return render(
    <I18nProvider>
      <AiSettings api={api} />
    </I18nProvider>,
  );
}
afterEach(() => {
  localStorage.removeItem('app:language');
});

describe('web AI settings', () => {
  it('shows the managed card without credential controls, all eight routes, and unknown cost as unknown', async () => {
    const h = fixture();
    settings(h.api);
    const provider = await screen.findByTestId('provider-operator');
    expect(provider).toHaveTextContent('Managed by the server');
    expect(provider).toHaveTextContent('vision-model');
    expect(provider.querySelector('input,button')).toBeNull();
    expect(await screen.findAllByRole('combobox')).toHaveLength(9);
    const usage = within(screen.getByTestId('ai-usage'));
    expect(await usage.findByText('—')).toBeInTheDocument();
    expect(usage.getByText('$0.03')).toBeInTheDocument();
    expect(usage.getByText(/does not mean zero cost/)).toBeInTheDocument();
    expect(h.api.usage).toHaveBeenCalledWith(30);
  });
  it('saves routes and all preferences, confirming only after the server answers', async () => {
    const h = fixture();
    settings(h.api);
    fireEvent.change(await screen.findByLabelText('Cataloging'), { target: { value: 'operator' } });
    fireEvent.change(screen.getByLabelText('Concurrent BYOK calls'), { target: { value: '8' } });
    fireEvent.click(screen.getByLabelText('Run vision quality checks'));
    fireEvent.click(screen.getByLabelText('Analyze websites automatically'));
    fireEvent.click(screen.getByLabelText('Show interim dictation results'));
    fireEvent.click(screen.getByRole('button', { name: 'Save preferences' }));
    await screen.findByText('Preferences saved.');
    expect(h.api.updateSettings).toHaveBeenCalledWith({
      ...DEFAULTS,
      aiRouting: { catalog: 'operator' },
      aiConcurrency: 8,
      aiVisionQc: true,
      aiAutoAnalyzeWebsites: true,
      aiDictationInterim: true,
    });
    expect(screen.getByRole('button', { name: 'Save preferences' })).toBeDisabled();
  });
  it('keeps unsaved changes and shows failure when the server refuses the save', async () => {
    const h = fixture();
    vi.mocked(h.api.updateSettings).mockRejectedValue({ code: 'network' });
    settings(h.api);
    fireEvent.click(await screen.findByLabelText('Generate search suggestions'));
    fireEvent.click(screen.getByRole('button', { name: 'Save preferences' }));
    await screen.findByRole('alert');
    expect(screen.queryByText('Preferences saved.')).toBeNull();
    expect(screen.getByLabelText('Generate search suggestions')).not.toBeChecked();
    expect(screen.getByRole('button', { name: 'Save preferences' })).toBeEnabled();
  });
  it('explains missing vision routes and translates the whole UI in Italian', async () => {
    const h = fixture([{ ...NODE, models: { ...NODE.models, vision: null } }]);
    settings(h.api, 'it');
    await screen.findByLabelText('Catalogazione');
    expect(screen.getAllByText(/richiedono un modello visivo/)).toHaveLength(2);
    expect(screen.getByLabelText('Chiamate BYOK simultanee')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Salva preferenze' })).toBeInTheDocument();
  });
  it('updates degraded/offline/invalid-key banners through SSE and clears on recovery, with cleanup', async () => {
    const h = fixture();
    localStorage.setItem('app:language', 'en');
    const view = render(
      <I18nProvider>
        <ProviderStatusBanner api={h.api} />
      </I18nProvider>,
    );
    await waitFor(() => expect(h.api.list).toHaveBeenCalled());
    await act(async () => {});
    act(() => h.emit('offline'));
    expect(await screen.findByTestId('provider-status-banner')).toHaveTextContent(
      'Your AI node is offline; work waits',
    );
    expect(screen.getByRole('link', { name: 'AI settings' })).toHaveAttribute(
      'href',
      '/settings/ai',
    );
    act(() => h.emit('degraded'));
    expect(screen.getByTestId('provider-status-banner')).toHaveTextContent('degraded');
    act(() => h.emit('invalid_key'));
    expect(screen.getByTestId('provider-status-banner')).toHaveTextContent('refused its key');
    act(() => h.emit('ok'));
    expect(screen.queryByTestId('provider-status-banner')).toBeNull();
    view.unmount();
    expect(h.listeners.size).toBe(0);
  });
  it('keeps the newest SSE status when an older provider snapshot finishes later', async () => {
    const h = fixture();
    let complete!: (providers: AiProviderSummary[]) => void;
    vi.mocked(h.api.list).mockImplementation(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    localStorage.setItem('app:language', 'en');
    render(
      <I18nProvider>
        <ProviderStatusBanner api={h.api} />
      </I18nProvider>,
    );
    act(() => h.emit('offline'));
    await act(async () => complete([NODE]));
    expect(await screen.findByTestId('provider-status-banner')).toHaveTextContent('offline');
  });

  it('keeps preferences available when usage fails and explains an account without providers', async () => {
    const h = fixture([]);
    vi.mocked(h.api.usage).mockRejectedValue({ code: 'network' });
    settings(h.api);
    await screen.findByLabelText('Cataloging');
    expect(screen.getByText(/No AI provider is configured/)).toBeInTheDocument();
    expect(within(screen.getByTestId('ai-usage')).getByRole('alert')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save preferences' })).toBeInTheDocument();
  });

  it('recovers missed status events from a resync snapshot', async () => {
    const h = fixture();
    localStorage.setItem('app:language', 'en');
    render(
      <I18nProvider>
        <ProviderStatusBanner api={h.api} />
      </I18nProvider>,
    );
    await act(async () => {});
    act(() => h.emit('offline'));
    await screen.findByTestId('provider-status-banner');
    act(() => h.resync());
    await waitFor(() => expect(screen.queryByTestId('provider-status-banner')).toBeNull());
    expect(h.api.list).toHaveBeenCalledTimes(2);
  });
});

it('uses account HTTP endpoints and sends only the allowlisted settings, gated by ai.tasks', async () => {
  const wire = {
    ...DEFAULTS,
    language: 'en',
    archiveAssetTypes: { image: true, video: true, thumbnail: true },
  };
  const http = {
    get: vi.fn(async (path: string) =>
      path.endsWith('/providers') ? [NODE] : path.endsWith('/usage/ai') ? { days: [] } : wire,
    ),
    send: vi.fn(async () => new Response(JSON.stringify(wire))),
    onUnauthorized: () => () => {},
  } as unknown as Http;
  const events = { on: vi.fn(() => () => {}), close: vi.fn() } as unknown as EventStream;
  const api = createAiProvidersApi(http, events);
  expect(await api.getSettings()).toEqual(DEFAULTS);
  await api.updateSettings({
    ...DEFAULTS,
    ...{ language: 'it', aiProviders: [{ key: 'never-send' }] },
  } as AiProviderSettings);
  expect(http.send).toHaveBeenCalledWith('PUT', '/api/v1/me/settings', DEFAULTS);
  await api.usage(7);
  expect(http.get).toHaveBeenCalledWith('/api/v1/me/usage/ai', new URLSearchParams({ days: '7' }));
  api.onStatus(() => {});
  api.onResync(() => {});
  expect(events.on).toHaveBeenCalledWith('provider.status', expect.any(Function));
  const capable = { ...OWNER, capabilities: { ...OWNER.capabilities, 'ai.tasks': true } };
  expect(createHttpClient(http, { me: capable, events }).aiProviders).toBeDefined();
  expect(
    createHttpClient(http, {
      me: { ...capable, capabilities: { ...capable.capabilities, 'ai.tasks': false } },
      events,
    }).aiProviders,
  ).toBeUndefined();
  expect(createHttpClient(http, { events }).aiProviders).toBeUndefined();
});
