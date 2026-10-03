// The shared UI primitives (UX audit §3.6, UX-2).
import React, { createRef } from 'react';
import { render, screen, fireEvent } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { Inbox, Plus, Trash2 } from 'lucide-react';
import {
  Button,
  EmptyState,
  IconButton,
  Notice,
  PageHeader,
  Spinner,
} from '../../../src/components/ui';

describe('Button', () => {
  it('is a type="button" secondary md button by default and forwards its ref', () => {
    const ref = createRef<HTMLButtonElement>();
    render(<Button ref={ref}>Save</Button>);
    const button = screen.getByRole('button', { name: 'Save' });
    expect(button).toHaveAttribute('type', 'button');
    expect(button.className).toContain('border-strong');
    expect(button.className).toContain('h-9');
    expect(button.className).toContain('narrow:min-h-11');
    expect(ref.current).toBe(button);
  });

  it('fills the primary variant with --accent-fill behind a white label', () => {
    render(
      <Button variant="primary" size="lg" icon={Plus}>
        Add
      </Button>,
    );
    const button = screen.getByRole('button', { name: 'Add' });
    expect(button.className).toContain('bg-accent-fill');
    expect(button.className).toContain('text-white');
    expect(button.className).toContain('h-11');
    expect(button.querySelector('svg')).not.toBeNull();
  });

  it('shows a spinner, sets aria-busy and blocks clicks while loading', () => {
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Saving
      </Button>,
    );
    const button = screen.getByRole('button', { name: 'Saving' });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute('aria-busy', 'true');
    expect(button.querySelector('.animate-spin')).not.toBeNull();
    fireEvent.click(button);
    expect(onClick).not.toHaveBeenCalled();
  });
});

describe('IconButton', () => {
  it('names itself from the required label (aria-label and tooltip)', () => {
    const onClick = vi.fn();
    render(<IconButton label="Delete post" icon={Trash2} tone="danger" onClick={onClick} />);
    const button = screen.getByRole('button', { name: 'Delete post' });
    expect(button).toHaveAttribute('title', 'Delete post');
    expect(button.className).toContain('hover:text-error');
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledOnce();
  });

  it('grows to 44px on narrow by default, or extends only its hit area', () => {
    const { rerender } = render(<IconButton label="Close" icon={Trash2} size="sm" />);
    expect(screen.getByRole('button').className).toContain('narrow:h-11');
    rerender(<IconButton label="Close" icon={Trash2} size="sm" narrow="hit" />);
    const button = screen.getByRole('button');
    expect(button.className).toContain('u-hit');
    expect(button.className).not.toContain('narrow:h-11');
    expect(button.className).toContain('h-7');
  });
});

describe('EmptyState', () => {
  it('shows the icon, title, body and actions', () => {
    const onAction = vi.fn();
    const onSecondary = vi.fn();
    render(
      <EmptyState
        testId="empty"
        icon={Inbox}
        title="Trash is empty"
        body="Deleted posts stay here for 30 days."
        action={{ label: 'Back to library', onClick: onAction, testId: 'empty-action' }}
        secondaryAction={{ label: 'Learn more', onClick: onSecondary }}
      />,
    );
    const root = screen.getByTestId('empty');
    expect(root.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
    expect(screen.getByText('Trash is empty').className).toContain('text-primary');
    expect(screen.getByText('Deleted posts stay here for 30 days.')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('empty-action'));
    fireEvent.click(screen.getByRole('button', { name: 'Learn more' }));
    expect(onAction).toHaveBeenCalledOnce();
    expect(onSecondary).toHaveBeenCalledOnce();
  });

  it('renders without a body or actions', () => {
    render(<EmptyState icon={Inbox} title="No background jobs" />);
    expect(screen.getByText('No background jobs')).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });
});

describe('Notice', () => {
  it('announces errors at once and the other tones politely, with an icon', () => {
    const { rerender } = render(<Notice tone="error">Could not save.</Notice>);
    expect(screen.getByRole('alert')).toHaveTextContent('Could not save.');
    for (const tone of ['info', 'ok', 'warn'] as const) {
      rerender(<Notice tone={tone}>Saved.</Notice>);
      const status = screen.getByRole('status');
      expect(status).toHaveTextContent('Saved.');
      expect(status.querySelector('svg')).not.toBeNull();
    }
  });
});

describe('Spinner', () => {
  it('is decorative without a label and a named status with one', () => {
    const { container, rerender } = render(<Spinner />);
    expect(screen.queryByRole('status')).toBeNull();
    expect(container.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
    rerender(<Spinner label="Loading posts" />);
    expect(screen.getByRole('status', { name: 'Loading posts' })).toBeInTheDocument();
  });
});

describe('PageHeader', () => {
  it('renders the leading slot, an h1 title, the count, a subtitle and actions', () => {
    render(
      <PageHeader
        testId="header"
        leading={<button type="button">Menu</button>}
        title="Trash"
        count={12}
        subtitle="Deleted posts stay here for 30 days."
        actions={<button type="button">Empty trash</button>}
      />,
    );
    expect(screen.getByRole('heading', { level: 1, name: 'Trash' })).toBeInTheDocument();
    expect(screen.getByText('12').className).toContain('text-muted');
    expect(screen.getByText('Deleted posts stay here for 30 days.')).toBeInTheDocument();
    const header = screen.getByTestId('header');
    const buttons = Array.from(header.querySelectorAll('button')).map((b) => b.textContent);
    expect(buttons).toEqual(['Menu', 'Empty trash']);
    expect(header.className).toContain('h-14');
  });

  it('can render the title as an h2', () => {
    render(<PageHeader title="Jobs" titleAs="h2" />);
    expect(screen.getByRole('heading', { level: 2, name: 'Jobs' })).toBeInTheDocument();
    expect(React.isValidElement(<PageHeader title="x" />)).toBe(true);
  });
});
