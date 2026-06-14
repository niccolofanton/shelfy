import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import CollectionModal, { COLLECTION_COLORS } from '../../src/components/CollectionModal';

describe('CollectionModal', () => {
  it('disables the save button until a name is entered', () => {
    render(<CollectionModal onClose={vi.fn()} onSave={vi.fn()} />);
    const save = screen.getByTestId('collection-save');
    expect(save).toBeDisabled();

    fireEvent.change(screen.getByTestId('collection-name-input'), { target: { value: 'Ricette' } });
    expect(save).not.toBeDisabled();
  });

  it('calls onSave with the trimmed name and default color, then onClose', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<CollectionModal onClose={onClose} onSave={onSave} />);

    fireEvent.change(screen.getByTestId('collection-name-input'), {
      target: { value: '  Viaggi  ' },
    });
    fireEvent.click(screen.getByTestId('collection-save'));

    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({ name: 'Viaggi', color: COLLECTION_COLORS[0] }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it('lets the user pick a different color', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<CollectionModal onClose={vi.fn()} onSave={onSave} />);

    fireEvent.change(screen.getByTestId('collection-name-input'), { target: { value: 'Idee' } });
    fireEvent.click(screen.getByTestId(`collection-color-${COLLECTION_COLORS[3]}`));
    fireEvent.click(screen.getByTestId('collection-save'));

    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({ name: 'Idee', color: COLLECTION_COLORS[3] }),
    );
  });

  it('prefills name and color when editing an existing collection', () => {
    render(
      <CollectionModal
        initial={{ name: 'Esistente', color: COLLECTION_COLORS[2] } as unknown as Shelfy.Collection}
        onClose={vi.fn()}
        onSave={vi.fn()}
      />,
    );
    expect(screen.getByTestId('collection-name-input')).toHaveValue('Esistente');
  });
});
