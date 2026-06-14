import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import ActivityCenter from '../../src/components/ActivityCenter';
import { ActivityProvider } from '../../src/hooks/useActivity';

function setup(sources = {}, props = {}) {
  const onAction = vi.fn();
  const onNavigate = vi.fn();
  const result = render(
    <ActivityProvider {...sources}>
      <ActivityCenter onAction={onAction} onNavigate={onNavigate} {...props} />
    </ActivityProvider>,
  );
  return { onAction, onNavigate, ...result };
}

describe('ActivityCenter', () => {
  it('shows the idle headline when nothing is happening', () => {
    setup();
    const strip = screen.getByTestId('activity-strip');
    expect(strip).toHaveTextContent('Attività');
    expect(strip).toHaveTextContent('Nessuna attività');
    expect(screen.queryByTestId('activity-badge')).toBeNull();
  });

  it('reflects the live count + headline on the strip when busy', () => {
    setup({ analysis: { jobs: [{ key: 'a', status: 'analyzing', description: 'Post uno' }] } });
    expect(screen.getByTestId('activity-strip')).toHaveTextContent('Auto-tag 0/1');
    expect(screen.getByTestId('activity-badge')).toHaveTextContent('1');
  });

  it('opens the popover and lists the in-progress activity', () => {
    setup({ analysis: { jobs: [{ key: 'a', status: 'analyzing', description: 'Post uno' }] } });
    expect(screen.queryByTestId('activity-popover')).toBeNull();
    fireEvent.click(screen.getByTestId('activity-strip'));
    const popover = screen.getByTestId('activity-popover');
    expect(within(popover).getByTestId('activity-live')).toBeInTheDocument();
    expect(within(popover).getByTestId('activity-item-analysis')).toBeInTheDocument();
  });

  it('shows the paused state on the strip and in the live row', () => {
    setup({
      analysis: {
        jobs: [{ key: 'a', status: 'analyzing', description: 'Post uno' }],
        paused: true,
      },
    });
    // Strip headline carries the paused suffix instead of reading "in corso".
    expect(screen.getByTestId('activity-strip')).toHaveTextContent('in pausa');
    fireEvent.click(screen.getByTestId('activity-strip'));
    const row = screen.getByTestId('activity-item-analysis');
    expect(row).toHaveTextContent('In pausa');
    // The toggle action now offers "Riprendi".
    expect(screen.getByTestId('activity-action-analysis-toggle')).toHaveTextContent('Riprendi');
  });

  it('routes a queue action to onAction', () => {
    const { onAction } = setup({ analysis: { jobs: [{ key: 'a', status: 'analyzing' }] } });
    fireEvent.click(screen.getByTestId('activity-strip'));
    fireEvent.click(screen.getByTestId('activity-action-analysis-cancel'));
    expect(onAction).toHaveBeenCalledWith(
      'analysis-cancel',
      expect.objectContaining({ kind: 'analysis' }),
    );
  });

  it('navigates to the relevant view when an item body is clicked', () => {
    const { onNavigate } = setup({
      downloads: { jobs: [{ key: '1', status: 'downloading', progress: 0.1 }] },
    });
    fireEvent.click(screen.getByTestId('activity-strip'));
    fireEvent.click(screen.getByTestId('activity-item-download'));
    expect(onNavigate).toHaveBeenCalledWith('downloads');
  });

  it('navigates a sync item to the browser on the right platform', () => {
    const { onNavigate } = setup({ sync: { syncing: { twitter: true }, counts: { twitter: 3 } } });
    fireEvent.click(screen.getByTestId('activity-strip'));
    fireEvent.click(screen.getByTestId('activity-item-sync:twitter'));
    expect(onNavigate).toHaveBeenCalledWith('browser', { platform: 'twitter' });
  });

  it('navigates from a recent log entry when clicked', () => {
    const { onNavigate, rerender } = setup({ save: { active: true, lastSave: null } });
    rerender(
      <ActivityProvider
        save={{ active: false, lastSave: { count: 2, platform: 'instagram', ts: 9 } }}
      >
        <ActivityCenter onAction={() => {}} onNavigate={onNavigate} />
      </ActivityProvider>,
    );
    fireEvent.click(screen.getByTestId('activity-strip'));
    fireEvent.click(within(screen.getByTestId('activity-recent')).getByText('Post salvati'));
    expect(onNavigate).toHaveBeenCalledWith('gallery');
  });

  it('does not flag infra "ready" events as unread (read by default)', () => {
    const { rerender } = render(
      <ActivityProvider model={{ status: { ready: false } }}>
        <ActivityCenter onAction={() => {}} onNavigate={() => {}} />
      </ActivityProvider>,
    );
    rerender(
      <ActivityProvider model={{ status: { ready: true } }}>
        <ActivityCenter onAction={() => {}} onNavigate={() => {}} />
      </ActivityProvider>,
    );
    // The "Modello AI pronto" entry is logged but must not light the unread dot.
    expect(screen.queryByTestId('activity-unread')).toBeNull();
    fireEvent.click(screen.getByTestId('activity-strip'));
    expect(
      within(screen.getByTestId('activity-popover')).getByText('Modello AI pronto'),
    ).toBeInTheDocument();
  });

  it('logs a recent entry when a finished save batch is reported', () => {
    const { rerender } = render(
      <ActivityProvider save={{ active: true, lastSave: null }}>
        <ActivityCenter onAction={() => {}} onNavigate={() => {}} />
      </ActivityProvider>,
    );
    // Save completes: active flips off and lastSave carries the count.
    rerender(
      <ActivityProvider
        save={{ active: false, lastSave: { count: 8, platform: 'instagram', ts: 123 } }}
      >
        <ActivityCenter onAction={() => {}} onNavigate={() => {}} />
      </ActivityProvider>,
    );
    fireEvent.click(screen.getByTestId('activity-strip'));
    const popover = screen.getByTestId('activity-popover');
    expect(within(popover).getByTestId('activity-recent')).toBeInTheDocument();
    expect(within(popover).getByText(/8 post aggiunti/)).toBeInTheDocument();
  });

  // Lo stato updater non arriva via props ma da una sottoscrizione propria del
  // provider (getUpdateState + onUpdaterState). Lo iniettiamo dal mock di
  // electronAPI così possiamo verificare come la striscia rende un update
  // "pronto" rispetto a uno "in lavorazione".
  function withUpdateState(state) {
    window.electronAPI.getUpdateState = vi.fn().mockResolvedValue(state);
    window.electronAPI.onUpdaterState = vi.fn().mockReturnValue(() => {});
  }

  it('shows the ready update as an attention state, NOT a perpetual spinner', async () => {
    // Regressione: un update scaricato/buildato ha progress null ma è PRONTO —
    // la striscia deve segnalare "azione richiesta", non continuare a girare come
    // se stesse ancora lavorando (il "rimane in pending" segnalato dall'utente).
    withUpdateState({ status: 'built', version: '1.4.0' });
    setup();
    const strip = await screen.findByTestId('activity-strip');
    expect(await screen.findByText('Aggiornamento 1.4.0 pronto')).toBeInTheDocument();
    // Nessuno spinner indeterminato…
    expect(strip.querySelector('.animate-spin')).toBeNull();
    // …ma un'icona in evidenza che invita ad agire.
    expect(strip.querySelector('.u-glow')).not.toBeNull();
    // E la CTA "Riavvia ora" è raggiungibile dal popover.
    fireEvent.click(strip);
    expect(screen.getByTestId('activity-action-update-install')).toHaveTextContent('Riavvia ora');
  });

  it('keeps the spinner while an update is genuinely in progress', async () => {
    // Contrasto: durante download/compilazione (running, progress null) lo
    // spinner DEVE restare — è la fase di lavoro vera.
    withUpdateState({ status: 'building', version: '1.4.0' });
    setup();
    const strip = await screen.findByTestId('activity-strip');
    expect(await screen.findByText('Compilazione aggiornamento')).toBeInTheDocument();
    expect(strip.querySelector('.animate-spin')).not.toBeNull();
  });
});
