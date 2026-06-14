import { describe, it, expect, vi } from 'vitest';
import { fetchAiSetup } from '../../src/hooks/useAiSetup';

// fetchAiSetup derives `complete` from the four pipeline pieces (binaries, VLM,
// STT, embeddings). The setup.js defaults paint everything ready.
describe('fetchAiSetup', () => {
  it('è completo quando binari e tutti i modelli sono pronti', async () => {
    const s = await fetchAiSetup();
    expect(s!.complete).toBe(true);
  });

  it('non è completo senza i binari runtime', async () => {
    vi.mocked(window.electronAPI.getBinariesStatus).mockResolvedValue({ ready: false, missing: 2 });
    const s = await fetchAiSetup();
    expect(s!.complete).toBe(false);
  });

  it('non è completo senza un modello di analisi scaricato', async () => {
    vi.mocked(window.electronAPI.listModels).mockResolvedValue([
      { id: 'qwen3vl-8b', ready: false, partial: false, active: true, downloading: false },
    ]);
    const s = await fetchAiSetup();
    expect(s!.complete).toBe(false);
  });

  it('non è completo senza il modello vocale', async () => {
    vi.mocked(window.electronAPI.sttListModels).mockResolvedValue([
      { id: 'whisper-turbo-q5', ready: false, active: true, downloading: false },
    ]);
    const s = await fetchAiSetup();
    expect(s!.complete).toBe(false);
  });

  it('una API che fallisce conta come non pronta, senza far esplodere il probe', async () => {
    vi.mocked(window.electronAPI.embListModels).mockRejectedValue(new Error('ipc down'));
    const s = await fetchAiSetup();
    expect(s!.complete).toBe(false);
    expect(s!.emb).toEqual([]);
  });
});
