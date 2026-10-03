import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  AiProviderInput,
  AiProvidersApi,
  AiProviderSummary,
  AiProviderTest,
} from '../../src/api/aiProviders';
import { I18nProvider } from '../../src/i18n';
import AiSettings from '../../src/views/settings/Ai';
import ProviderStatusBanner from '../../src/components/ai/ProviderStatusBanner';
import { requestProviderConnection } from '../../src/components/ai/providerConnection';
const KEY = 'synthetic_UI_credential_1234';
const CHECKS: AiProviderTest = {
  testedAt: 1,
  models: { ok: true, skipped: false, error: null },
  text: { ok: true, skipped: false, error: null },
  vision: { ok: true, skipped: false, error: null },
  schema: { ok: true, skipped: false, error: null },
};
function fixture(initial: AiProviderSummary[] = []) {
  let providers = structuredClone(initial);
  let preferences = {
    aiRouting: {},
    aiConcurrency: 4,
    aiSuggestions: true,
    aiVisionQc: false,
    aiAutoAnalyzeWebsites: false,
    aiDictationInterim: false,
  };
  const management = {
    save: vi.fn(async (id: string, input: AiProviderInput) => {
      const old = providers.find((p) => p.id === id);
      providers = providers.filter((p) => p.id !== id);
      providers.push({
        id,
        kind: input.kind,
        label: input.label,
        baseUrl: input.baseUrl,
        taskModels: input.models,
        prices: input.prices,
        models: {
          text: input.models.chat ?? null,
          vision: input.models.catalog ?? null,
          embed: input.models.embed ?? null,
        },
        stt: !!input.models.stt,
        managed: false,
        status: 'ok',
        configured: true,
        last4: input.key?.slice(-4) ?? old?.last4,
        consentVersion: 'test-consent-version',
      });
    }),
    delete: vi.fn(async (id: string) => {
      providers = providers.filter((p) => p.id !== id);
    }),
    test: vi.fn(async (id: string) => {
      const provider = providers.find((p) => p.id === id)!;
      provider.test = structuredClone(CHECKS);
      return structuredClone(CHECKS);
    }),
    consent: vi.fn(async (id: string, version: string) => {
      providers.find((p) => p.id === id)!.consent = { version, acceptedAt: 1 };
    }),
  };
  const api: AiProvidersApi = {
    management,
    list: vi.fn(async () => structuredClone(providers)),
    getSettings: vi.fn(async () => structuredClone(preferences)),
    updateSettings: vi.fn(async (value) => {
      preferences = value;
      return value;
    }),
    usage: vi.fn(async () => []),
    onStatus: () => () => {},
    onResync: () => () => {},
  };
  return {
    api,
    management,
    get providers() {
      return providers;
    },
  };
}
function view(api: AiProvidersApi) {
  localStorage.setItem('app:language', 'en');
  return render(
    <I18nProvider>
      <ProviderStatusBanner api={api} />
      <AiSettings api={api} />
    </I18nProvider>,
  );
}
async function keyStep() {
  fireEvent.click(await screen.findByRole('button', { name: 'Add provider' }));
  let dialog = within(await screen.findByRole('dialog'));
  fireEvent.change(dialog.getByLabelText('Provider name'), {
    target: { value: 'My synthetic provider' },
  });
  fireEvent.change(dialog.getByLabelText('Base URL'), {
    target: { value: 'https://provider.example.test/v1' },
  });
  fireEvent.click(dialog.getByRole('button', { name: 'Continue' }));
  dialog = within(screen.getByRole('dialog'));
  fireEvent.change(dialog.getByLabelText('API key'), { target: { value: KEY } });
  fireEvent.change(dialog.getByLabelText('Chat'), { target: { value: 'typed-chat' } });
  fireEvent.change(dialog.getByLabelText('Cataloging'), { target: { value: 'typed-vision' } });
  return dialog;
}
afterEach(() => localStorage.removeItem('app:language'));
describe('BYOK connection wizard', () => {
  it('saves a write-only key, runs synthetic checks before explicit consent, then activates a route', async () => {
    const h = fixture();
    view(h.api);
    fireEvent.click(await screen.findByLabelText('Generate search suggestions'));
    let dialog = await keyStep();
    fireEvent.change(dialog.getByLabelText('Input price (USD / million tokens)'), {
      target: { value: '0.25' },
    });
    fireEvent.change(dialog.getByLabelText('Output price (USD / million tokens)'), {
      target: { value: '0.75' },
    });
    fireEvent.click(dialog.getByRole('button', { name: 'Save provider' }));
    await screen.findByText(
      'These checks send only synthetic text and a tiny generated image. No library content is sent.',
    );
    expect(document.body.innerHTML).not.toContain(KEY);
    expect(document.querySelector('input[type="password"]')).toBeNull();
    expect(localStorage.getItem(KEY)).toBeNull();
    expect(h.management.save).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        key: KEY,
        models: { chat: 'typed-chat', catalog: 'typed-vision' },
        prices: { inputPerMillionUsd: 0.25, outputPerMillionUsd: 0.75 },
      }),
    );
    expect(h.management.consent).not.toHaveBeenCalled();
    dialog = within(screen.getByRole('dialog'));
    expect(dialog.getByRole('button', { name: 'Continue' })).toBeDisabled();
    fireEvent.click(dialog.getByRole('button', { name: 'Run synthetic test' }));
    await waitFor(() => expect(dialog.getByRole('button', { name: 'Continue' })).toBeEnabled());
    expect(h.management.test).toHaveBeenCalled();
    expect(h.management.consent).not.toHaveBeenCalled();
    fireEvent.click(dialog.getByRole('button', { name: 'Continue' }));
    const consent = dialog.getByRole('button', { name: 'Record consent' });
    expect(consent).toBeDisabled();
    fireEvent.click(dialog.getByLabelText('I consent to sending my content to this provider.'));
    fireEvent.click(consent);
    await screen.findByLabelText('Task to connect');
    expect(h.management.consent).toHaveBeenCalledWith(h.providers[0].id, 'test-consent-version');
    fireEvent.click(dialog.getByRole('button', { name: 'Activate route' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(h.api.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ aiRouting: { chat: h.providers[0].id } }),
    );
    expect(screen.getByLabelText('Generate search suggestions')).not.toBeChecked();
    expect(document.body.innerHTML).not.toContain(KEY);
  });
  it('keeps the dialog open and consent unavailable when the provider refuses the key', async () => {
    const h = fixture();
    h.management.test.mockImplementation(async () => {
      h.providers[0].status = 'invalid_key';
      return { ...CHECKS, text: { ok: false, skipped: false, error: 'invalid_key' } };
    });
    view(h.api);
    let dialog = await keyStep();
    fireEvent.click(dialog.getByRole('button', { name: 'Save provider' }));
    await screen.findByRole('button', { name: 'Run synthetic test' });
    dialog = within(screen.getByRole('dialog'));
    fireEvent.click(dialog.getByRole('button', { name: 'Run synthetic test' }));
    await screen.findByText(/Failed.*Invalid credential/);
    expect(dialog.getByRole('button', { name: 'Continue' })).toBeDisabled();
    expect(await screen.findByTestId('provider-status-banner')).toHaveTextContent(
      'refused its key',
    );
    expect(h.management.consent).not.toHaveBeenCalled();
    expect(document.body.innerHTML).not.toContain(KEY);
  });
  it('preserves all per-task models when editing without replacing a key and removes provider routing on delete', async () => {
    const custom: AiProviderSummary = {
      id: 'existing',
      stt: true,
      kind: 'anthropic',
      label: 'Existing provider',
      managed: false,
      status: 'ok',
      configured: true,
      last4: '4321',
      baseUrl: 'https://provider.example.test',
      models: { text: 'chat', vision: 'vision', embed: 'embedding' },
      taskModels: {
        chat: 'chat',
        catalog: 'vision',
        qc: 'qc',
        cluster: 'cluster',
        alias: 'alias',
        embed: 'embedding',
        stt: 'stt',
        suggest: 'suggest',
      },
    };
    const h = fixture([custom]);
    view(h.api);
    fireEvent.click(await screen.findByRole('button', { name: 'Edit provider' }));
    const dialog = within(await screen.findByRole('dialog'));
    expect(dialog.getByLabelText('API key')).toHaveValue('');
    expect(dialog.getByLabelText('Tag aliases')).toHaveValue('alias');
    fireEvent.click(dialog.getByRole('button', { name: 'Save provider' }));
    await screen.findByRole('button', { name: 'Run synthetic test' });
    expect(h.management.save.mock.calls[0][1]).not.toHaveProperty('key');
    expect(h.management.save.mock.calls[0][1].models).toEqual(custom.taskModels);
    fireEvent.click(dialog.getByRole('button', { name: 'Close' }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete provider' }));
    await waitFor(() => expect(screen.queryByTestId('provider-existing')).toBeNull());
    expect(h.management.delete).toHaveBeenCalledWith('existing');
  });
  it('opens for a task without a route through the shared connection seam', async () => {
    const h = fixture();
    view(h.api);
    await screen.findByRole('button', { name: 'Add provider' });
    act(() => requestProviderConnection('catalog'));
    expect(await screen.findByRole('dialog')).toHaveTextContent(
      'Connect a provider for Cataloging.',
    );
  });
  it('discards a pasted credential on failed save and keeps it out of errors', async () => {
    const h = fixture();
    h.management.save.mockRejectedValue({ code: 'ai_vault_disabled' });
    view(h.api);
    const dialog = await keyStep();
    fireEvent.click(dialog.getByRole('button', { name: 'Save provider' }));
    await screen.findByRole('alert');
    expect(dialog.getByLabelText('API key')).toHaveValue('');
    expect(document.body.innerHTML).not.toContain(KEY);
  });
});
