import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { vi } from 'vitest';
import AiOnboarding from '../../src/views/AiOnboarding';

// Catalog builders mirroring the listModels() shape from analyzer/stt/embeddings.
const vlmCatalog = (patch = {}) => [
  {
    id: 'qwen3vl-4b',
    name: 'Qwen3-VL 4B',
    tier: 'Veloce',
    note: 'leggero',
    sizeGB: 3.3,
    minRamGB: 8,
    recommended: false,
    ready: false,
    partial: false,
    active: false,
    downloading: false,
    ...(patch['qwen3vl-4b'] || {}),
  },
  {
    id: 'qwen3vl-8b',
    name: 'Qwen3-VL 8B',
    tier: 'Bilanciato',
    note: 'bilanciato',
    sizeGB: 6.2,
    minRamGB: 16,
    recommended: true,
    ready: false,
    partial: false,
    active: true,
    downloading: false,
    ...(patch['qwen3vl-8b'] || {}),
  },
];

const sttCatalog = (ready = false) => [
  {
    id: 'whisper-turbo-q5',
    name: 'Whisper Large v3 Turbo (q5)',
    tier: 'Qualità leggera',
    note: '',
    sizeGB: 0.55,
    sizeLabel: '547 MB',
    recommended: true,
    ready,
    partial: false,
    active: true,
    downloading: false,
  },
];

const embCatalog = (ready = false) => [
  {
    id: 'e5-small',
    name: 'multilingual-e5-small',
    tier: 'Embedding',
    note: '',
    sizeGB: 0.12,
    sizeLabel: '120 MB',
    recommended: true,
    ready,
    partial: false,
    active: true,
    downloading: false,
  },
];

// Nothing installed: the state a brand-new machine starts from.
function mockIncomplete() {
  window.electronAPI.listModels.mockResolvedValue(vlmCatalog());
  window.electronAPI.sttListModels.mockResolvedValue(sttCatalog());
  window.electronAPI.embListModels.mockResolvedValue(embCatalog());
  window.electronAPI.getBinariesStatus.mockResolvedValue({ ready: false, missing: 4 });
}

// Everything on disk: what refreshAll() finds after the install chain ran.
function mockComplete() {
  window.electronAPI.listModels.mockResolvedValue(vlmCatalog({ 'qwen3vl-8b': { ready: true } }));
  window.electronAPI.sttListModels.mockResolvedValue(sttCatalog(true));
  window.electronAPI.embListModels.mockResolvedValue(embCatalog(true));
  window.electronAPI.getBinariesStatus.mockResolvedValue({ ready: true, missing: 0 });
}

describe('AiOnboarding — wizard iniziale', () => {
  beforeEach(mockIncomplete);

  it('mostra hardware rilevato e preseleziona il modello consigliato per le spec', async () => {
    render(<AiOnboarding />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-hw')).toBeInTheDocument());
    // Hardware chips from getHardwareInfo (setup.js defaults: 8 core / 16 GB / Metal).
    expect(screen.getByText('8 core')).toBeInTheDocument();
    expect(screen.getByText('16 GB')).toBeInTheDocument();
    expect(screen.getByText('Apple Metal')).toBeInTheDocument();
    // Recommended model (hardware-fit) preselected and badged.
    const selected = screen.getByTestId('ai-onb-model-qwen3vl-8b');
    expect(selected).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByText('Consigliato per il tuo computer')).toBeInTheDocument();
    // CTA carries the missing volume.
    expect(screen.getByTestId('ai-onb-install').textContent).toContain('Scarica e configura tutto');
  });

  it('le alternative sono nascoste e si espandono dal toggle', async () => {
    render(<AiOnboarding />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-install')).toBeInTheDocument());
    expect(screen.queryByTestId('ai-onb-model-qwen3vl-4b')).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId('ai-onb-alts-toggle'));
    const alt = await screen.findByTestId('ai-onb-model-qwen3vl-4b');
    fireEvent.click(alt);
    // Selecting an alternative promotes it to the main (selected) card.
    await waitFor(() =>
      expect(screen.getByTestId('ai-onb-model-qwen3vl-4b')).toHaveAttribute('aria-pressed', 'true'),
    );
  });

  it('installa tutto in sequenza e mostra la schermata di successo', async () => {
    render(<AiOnboarding />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-install')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-onb-install'));
    // From here on, the catalogs read back as installed (the chain's refresh).
    mockComplete();
    await waitFor(() => expect(screen.getByTestId('ai-onb-success')).toBeInTheDocument());
    expect(window.electronAPI.downloadModel).toHaveBeenCalledWith('qwen3vl-8b');
    expect(window.electronAPI.sttDownloadModel).toHaveBeenCalledWith('whisper-turbo-q5');
    expect(window.electronAPI.ensureBinaries).toHaveBeenCalled();
    expect(window.electronAPI.embDownloadModel).toHaveBeenCalledWith('e5-small');
  });

  it('salta i pezzi già installati', async () => {
    window.electronAPI.listModels.mockResolvedValue(vlmCatalog({ 'qwen3vl-8b': { ready: true } }));
    window.electronAPI.getBinariesStatus.mockResolvedValue({ ready: true, missing: 0 });
    render(<AiOnboarding />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-install')).toBeInTheDocument());
    expect(screen.getByText('Qwen3-VL 8B è già installato.')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('ai-onb-install'));
    mockComplete();
    await waitFor(() => expect(screen.getByTestId('ai-onb-success')).toBeInTheDocument());
    expect(window.electronAPI.downloadModel).not.toHaveBeenCalled();
    expect(window.electronAPI.ensureBinaries).not.toHaveBeenCalled();
    expect(window.electronAPI.sttDownloadModel).toHaveBeenCalledWith('whisper-turbo-q5');
    expect(window.electronAPI.embDownloadModel).toHaveBeenCalledWith('e5-small');
  });

  it('un errore ferma la catena e offre il retry', async () => {
    window.electronAPI.downloadModel.mockRejectedValue(new Error('rete giù'));
    render(<AiOnboarding />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-install')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-onb-install'));
    await waitFor(() => expect(screen.getByTestId('ai-onb-retry')).toBeInTheDocument());
    expect(screen.getByText('rete giù')).toBeInTheDocument();
    // The chain stopped at the first item: nothing after it was attempted.
    expect(window.electronAPI.sttDownloadModel).not.toHaveBeenCalled();
    // Retry re-runs the plan once the failure is resolved.
    window.electronAPI.downloadModel.mockResolvedValue({ ready: true });
    fireEvent.click(screen.getByTestId('ai-onb-retry'));
    mockComplete();
    await waitFor(() => expect(screen.getByTestId('ai-onb-success')).toBeInTheDocument());
    expect(window.electronAPI.downloadModel).toHaveBeenCalledTimes(2);
  });

  it('skip e impostazioni avanzate chiamano i rispettivi callback', async () => {
    const onSkip = vi.fn();
    const onOpenSettings = vi.fn();
    render(<AiOnboarding onSkip={onSkip} onOpenSettings={onOpenSettings} />);
    await waitFor(() => expect(screen.getByTestId('ai-onb-skip')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-onb-skip'));
    expect(onSkip).toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('ai-onb-settings'));
    expect(onOpenSettings).toHaveBeenCalled();
  });
});

describe('AiOnboarding — successo', () => {
  it('la CTA finale avvia l’analisi e chiude il gate', async () => {
    mockComplete();
    const onDone = vi.fn();
    render(<AiOnboarding onDone={onDone} />);
    // Pipeline already complete → straight to the success screen.
    await waitFor(() => expect(screen.getByTestId('ai-onb-success')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-onb-start'));
    await waitFor(() => expect(window.electronAPI.analyzeMissing).toHaveBeenCalled());
    expect(onDone).toHaveBeenCalled();
  });
});
