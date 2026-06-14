import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { ReactNode } from 'react';
import FilterBar from '../../src/components/FilterBar';

interface TestFilters {
  search: string;
  platform: string;
  mediaType: string;
  downloadStatus: string;
  aiTagged: string;
  tag?: string;
}

const defaultFilters: TestFilters = {
  search: '',
  platform: 'all',
  mediaType: 'all',
  downloadStatus: 'all',
  aiTagged: 'all',
};

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

interface SetupProps {
  filters?: TestFilters;
  total?: number;
  drawerOpen?: boolean;
  leading?: ReactNode;
  trailing?: ReactNode;
}

function setup(props: SetupProps = {}) {
  const onFiltersChange = vi.fn<[TestFilters], void>();
  const onToggleDrawer = vi.fn();
  const result = render(
    <FilterBar
      filters={defaultFilters}
      onChange={onFiltersChange}
      total={42}
      onToggleDrawer={onToggleDrawer}
      {...props}
    />,
  );
  return { onFiltersChange, onToggleDrawer, ...result };
}

describe('FilterBar', () => {
  describe('rendering', () => {
    it('displays total count in the format "N post"', () => {
      setup({ total: 123 });
      expect(screen.getByText('123 post')).toBeInTheDocument();
    });

    it('renders the Filtri toggle button', () => {
      setup();
      expect(screen.getByTestId('filters-toggle')).toHaveTextContent('Filtri');
    });

    it('no longer renders the source/platform pills (moved to the drawer)', () => {
      setup();
      expect(screen.queryByRole('button', { name: 'Instagram' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'X / Twitter' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Siti' })).toBeNull();
    });

    it('renders the grid zoom control with the localized tooltips', () => {
      setup();
      const zoom = screen.getByTestId('grid-size-control');
      expect(within(zoom).getByLabelText('Riduci dimensione griglia')).toBeInTheDocument();
      expect(within(zoom).getByLabelText('Aumenta dimensione griglia')).toBeInTheDocument();
    });

    it('renders the leading and trailing slots (Gallery-owned controls)', () => {
      setup({
        leading: <span data-testid="slot-leading">sort</span>,
        trailing: <span data-testid="slot-trailing">refresh</span>,
      });
      expect(screen.getByTestId('slot-leading')).toBeInTheDocument();
      expect(screen.getByTestId('slot-trailing')).toBeInTheDocument();
    });
  });

  describe('Filtri toggle', () => {
    it('calls onToggleDrawer when clicked', () => {
      const { onToggleDrawer } = setup();
      fireEvent.click(screen.getByTestId('filters-toggle'));
      expect(onToggleDrawer).toHaveBeenCalledTimes(1);
    });

    it('reflects the open drawer via aria-expanded', () => {
      setup({ drawerOpen: true });
      expect(screen.getByTestId('filters-toggle')).toHaveAttribute('aria-expanded', 'true');
    });

    it('shows an active-count badge when non-default filters are set', () => {
      setup({ filters: { ...defaultFilters, mediaType: 'video', downloadStatus: 'downloaded' } });
      expect(screen.getByTestId('filters-toggle')).toHaveTextContent('2');
    });
  });

  describe('active tag chip', () => {
    it('does not render the tag chip when filters.tag is empty', () => {
      setup();
      expect(screen.queryByTestId('tag-filter-chip')).toBeNull();
    });

    it('renders a removable chip when filters.tag is set', () => {
      setup({ filters: { ...defaultFilters, tag: 'sunset' } });
      const chip = screen.getByTestId('tag-filter-chip');
      expect(chip).toHaveTextContent('#sunset');
    });

    it('clicking the chip X clears the tag filter', () => {
      const { onFiltersChange } = setup({ filters: { ...defaultFilters, tag: 'sunset' } });
      const chip = screen.getByTestId('tag-filter-chip');
      fireEvent.click(within(chip).getByRole('button'));
      expect(onFiltersChange).toHaveBeenCalledWith(expect.objectContaining({ tag: undefined }));
    });
  });

  describe('search input debounce', () => {
    it('does NOT call onFiltersChange immediately when typing', () => {
      const { onFiltersChange } = setup();
      const input = screen.getByPlaceholderText('Search posts...');
      fireEvent.change(input, { target: { value: 'h' } });
      expect(onFiltersChange).not.toHaveBeenCalled();
    });

    it('calls onFiltersChange after 300ms with the typed search value', () => {
      const { onFiltersChange } = setup();
      const input = screen.getByPlaceholderText('Search posts...');
      fireEvent.change(input, { target: { value: 'hello' } });
      expect(onFiltersChange).not.toHaveBeenCalled();
      vi.advanceTimersByTime(300);
      expect(onFiltersChange).toHaveBeenCalledWith({ ...defaultFilters, search: 'hello' });
    });

    it('debounces multiple changes into a single call', () => {
      const { onFiltersChange } = setup();
      const input = screen.getByPlaceholderText('Search posts...');
      fireEvent.change(input, { target: { value: 'a' } });
      fireEvent.change(input, { target: { value: 'ab' } });
      fireEvent.change(input, { target: { value: 'abc' } });
      expect(onFiltersChange).not.toHaveBeenCalled();
      vi.advanceTimersByTime(300);
      const searchCalls = onFiltersChange.mock.calls.filter(([arg]) => arg.search !== undefined);
      expect(searchCalls.length).toBe(1);
      expect(searchCalls[0][0].search).toBe('abc');
    });
  });
});
