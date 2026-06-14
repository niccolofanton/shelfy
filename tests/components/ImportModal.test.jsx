import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import ImportModal from '../../src/components/ImportModal';

function setup(props = {}) {
  const onClose = vi.fn();
  const onImported = vi.fn();
  const result = render(<ImportModal onClose={onClose} onImported={onImported} {...props} />);
  return { onClose, onImported, ...result };
}

describe('ImportModal', () => {
  describe('idle state', () => {
    it('renders "Importa export JSON" heading', () => {
      setup();
      expect(screen.getByText('Importa export JSON')).toBeInTheDocument();
    });

    it('shows Choose File and Import buttons', () => {
      setup();
      expect(screen.getByRole('button', { name: /scegli file/i })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /^importa$/i })).toBeInTheDocument();
    });

    it('Import button is disabled when no file chosen', () => {
      setup();
      expect(screen.getByRole('button', { name: /^importa$/i })).toBeDisabled();
    });

    it('Cancel button calls onClose', () => {
      const { onClose } = setup();
      fireEvent.click(screen.getByRole('button', { name: /annulla/i }));
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('clicking overlay calls onClose in idle state', () => {
      const { onClose, container } = setup();
      // The outermost div is the overlay
      fireEvent.click(container.firstChild);
      expect(onClose).toHaveBeenCalledTimes(1);
    });
  });

  describe('idle → file selected', () => {
    it('clicking Choose File calls window.electronAPI.openFile()', async () => {
      window.electronAPI.openFile.mockResolvedValue(null);
      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      expect(window.electronAPI.openFile).toHaveBeenCalledTimes(1);
    });

    it('shows filename in code element after file is selected', async () => {
      window.electronAPI.openFile.mockResolvedValue('/Users/USERNAME/exports/saved-posts.json');
      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      const codeEl = await screen.findByText('saved-posts.json');
      expect(codeEl.tagName).toBe('CODE');
    });

    it('Import button becomes enabled after file is selected', async () => {
      window.electronAPI.openFile.mockResolvedValue('/Users/USERNAME/exports/saved-posts.json');
      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('saved-posts.json');
      expect(screen.getByRole('button', { name: /^importa$/i })).not.toBeDisabled();
    });

    it('does not show filename when openFile returns null', async () => {
      window.electronAPI.openFile.mockResolvedValue(null);
      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      // Wait a tick for async resolution
      await waitFor(() => {
        expect(screen.queryByRole('code')).toBeNull();
      });
    });
  });

  describe('idle → importing', () => {
    it('clicking Import calls window.electronAPI.importJSON with the file path', async () => {
      const filePath = '/Users/USERNAME/exports/saved-posts.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      // Keep importJSON pending so we can observe the importing state
      window.electronAPI.importJSON.mockReturnValue(new Promise(() => {}));

      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('saved-posts.json');

      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      expect(window.electronAPI.importJSON).toHaveBeenCalledWith(filePath);
    });

    it('shows "Importazione dei post…" during import', async () => {
      const filePath = '/Users/USERNAME/exports/saved-posts.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      window.electronAPI.importJSON.mockReturnValue(new Promise(() => {}));

      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('saved-posts.json');

      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      expect(await screen.findByText('Importazione dei post…')).toBeInTheDocument();
    });

    it('clicking overlay during importing does NOT call onClose', async () => {
      const filePath = '/path/to/file.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      window.electronAPI.importJSON.mockReturnValue(new Promise(() => {}));

      const { onClose, container } = setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('file.json');
      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      await screen.findByText('Importazione dei post…');

      fireEvent.click(container.firstChild);
      expect(onClose).not.toHaveBeenCalled();
    });
  });

  describe('importing → done', () => {
    async function setupDoneState() {
      const filePath = '/Users/USERNAME/exports/saved-posts.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      window.electronAPI.importJSON.mockResolvedValue({ imported: 5 });

      const handlers = setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('saved-posts.json');
      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      return handlers;
    }

    it('shows "Imported 5 new posts" after importJSON resolves', async () => {
      await setupDoneState();
      expect(await screen.findByText('Importati 5 nuovi post')).toBeInTheDocument();
    });

    it('clicking Close calls onClose', async () => {
      const { onClose } = await setupDoneState();
      const closeBtn = await screen.findByRole('button', { name: /^chiudi$/i });
      fireEvent.click(closeBtn);
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('onImported was called after successful import', async () => {
      const { onImported } = await setupDoneState();
      await screen.findByText('Importati 5 nuovi post');
      expect(onImported).toHaveBeenCalledTimes(1);
    });

    it('uses singular "post" when imported count is 1', async () => {
      const filePath = '/path/to/file.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      window.electronAPI.importJSON.mockResolvedValue({ imported: 1 });

      setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('file.json');
      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      expect(await screen.findByText('Importato 1 nuovo post')).toBeInTheDocument();
    });
  });

  describe('importing → error', () => {
    async function setupErrorState(message = 'Failed to parse JSON') {
      const filePath = '/Users/USERNAME/exports/bad-file.json';
      window.electronAPI.openFile.mockResolvedValue(filePath);
      window.electronAPI.importJSON.mockRejectedValue(new Error(message));

      const handlers = setup();
      fireEvent.click(screen.getByRole('button', { name: /scegli file/i }));
      await screen.findByText('bad-file.json');
      fireEvent.click(screen.getByRole('button', { name: /^importa$/i }));
      return handlers;
    }

    it('shows error message after importJSON rejects', async () => {
      await setupErrorState('Failed to parse JSON');
      expect(await screen.findByText('Failed to parse JSON')).toBeInTheDocument();
    });

    it('shows "Try Again" button in error state', async () => {
      await setupErrorState();
      expect(await screen.findByRole('button', { name: /riprova/i })).toBeInTheDocument();
    });

    it('clicking Try Again goes back to idle state', async () => {
      await setupErrorState();
      const tryAgainBtn = await screen.findByRole('button', { name: /riprova/i });
      fireEvent.click(tryAgainBtn);
      expect(screen.getByText('Importa export JSON')).toBeInTheDocument();
    });
  });
});
