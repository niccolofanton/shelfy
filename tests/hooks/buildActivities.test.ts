import { describe, it, expect } from 'vitest';
import { buildActivities } from '../../src/hooks/useActivity';
import type { AnalyzeJob } from '../../src/hooks/useAnalysis';

describe('buildActivities', () => {
  it('returns no live items and an idle headline when everything is quiet', () => {
    const { live, summary } = buildActivities({});
    expect(live).toEqual([]);
    expect(summary.activeCount).toBe(0);
    expect(summary.headline).toBe('Nessuna attività');
    expect(summary.hasError).toBe(false);
  });

  it('normalizes a running analysis queue into one aggregated item', () => {
    const jobs: AnalyzeJob[] = [
      { key: 'a', status: 'analyzing', description: 'Post uno' },
      { key: 'b', status: 'pending' },
      { key: 'c', status: 'done', durationMs: 1000, finishedAt: 1 },
    ];
    const { live, summary } = buildActivities({ analysis: { jobs, concurrency: 1 } });
    const item = live.find((i) => i.kind === 'analysis');
    expect(item).toBeTruthy();
    expect(item!.status).toBe('running');
    expect(item!.count).toBe(1); // one done
    expect(item!.total).toBe(3); // 2 active + 1 done
    expect(item!.subtitle).toBe('Post uno');
    expect(item!.actions!.map((a) => a.id)).toContain('analysis-cancel');
    expect(summary.headline).toBe('Auto-tag 1/3');
  });

  it('marks the analysis item paused when the queue is paused', () => {
    const jobs: AnalyzeJob[] = [{ key: 'a', status: 'analyzing' }];
    const { live, summary } = buildActivities({ analysis: { jobs, paused: true } });
    expect(live[0].status).toBe('paused');
    expect(live[0].actions!.find((a) => a.id === 'analysis-toggle')!.label).toBe('Riprendi');
    // The strip must read as paused, not "in corso": headline + primaryStatus reflect it.
    expect(live[0].short).toMatch(/in pausa$/);
    expect(summary.headline).toMatch(/in pausa$/);
    expect(summary.primaryStatus).toBe('paused');
  });

  it('aggregates the download queue with overall progress', () => {
    const jobs = [
      { key: '1', status: 'done' },
      { key: '2', status: 'downloading', progress: 0.5, authorUsername: 'mario' },
      { key: '3', status: 'pending' },
    ];
    const { live } = buildActivities({ downloads: { jobs } });
    const item = live.find((i) => i.kind === 'download');
    expect(item!.count).toBe(1);
    expect(item!.total).toBe(3);
    expect(item!.progress).toBeCloseTo((1 + 0.5) / 3); // done + partial / total
    expect(item!.subtitle).toBe('@mario');
  });

  it('omits the download item when the queue is fully drained', () => {
    const jobs = [
      { key: '1', status: 'done' },
      { key: '2', status: 'done' },
    ];
    const { live } = buildActivities({ downloads: { jobs } });
    expect(live.find((i) => i.kind === 'download')).toBeUndefined();
  });

  it('shows the model download with progress', () => {
    const { live } = buildActivities({ model: { progress: { progress: 0.42, label: 'gguf' } } });
    const item = live.find((i) => i.kind === 'model');
    expect(item!.progress).toBe(0.42);
    expect(item!.subtitle).toBe('gguf');
  });

  it('emits one sync item per syncing platform with its captured count', () => {
    const { live } = buildActivities({
      sync: { syncing: { instagram: true, twitter: false }, counts: { instagram: 12 } },
    });
    const syncs = live.filter((i) => i.kind === 'sync');
    expect(syncs).toHaveLength(1);
    expect(syncs[0].platform).toBe('instagram');
    expect(syncs[0].subtitle).toBe('12 nuovi post');
  });

  it('replaces the legacy sync item with the rich source-sync job for that platform', () => {
    const { live } = buildActivities({
      sync: {
        syncing: { instagram: true },
        counts: { instagram: 7 },
        jobs: {
          instagram: {
            status: 'syncing',
            currentLabel: 'Motivational',
            stepIndex: 1,
            stepCount: 3,
            scanned: 120,
            fresh: 12,
          },
        },
      },
    });
    const syncs = live.filter((i) => i.kind === 'sync');
    expect(syncs).toHaveLength(1); // job item only, no legacy duplicate
    expect(syncs[0].subtitle).toBe('Motivational · 2/3 · 120 analizzati · 12 nuovi');
    expect(syncs[0].actions!.map((a) => a.id)).toEqual(['sourcesync-stop']);
  });

  it('labels the all-saved step and omits the step counter on single-step runs', () => {
    const { live } = buildActivities({
      sync: {
        jobs: {
          twitter: { status: 'navigating', currentLabel: null, stepIndex: 0, stepCount: 1 },
        },
      },
    });
    const item = live.find((i) => i.kind === 'sync');
    expect(item!.subtitle).toBe('Tutti i salvati · 0 analizzati · 0 nuovi');
  });

  it('keeps a failed source-sync as an actionable error item with the login CTA', () => {
    const { live, summary } = buildActivities({
      sync: { jobs: { instagram: { status: 'error', error: 'login' } } },
    });
    const item = live.find((i) => i.kind === 'sync');
    expect(item!.status).toBe('error');
    expect(item!.title).toBe('Accesso richiesto — Instagram');
    expect(item!.actions!.map((a) => a.id)).toEqual(['sourcesync-open', 'sourcesync-dismiss']);
    expect(summary.hasError).toBe(true);
  });

  it('surfaces the save-in-progress item', () => {
    const { live } = buildActivities({ save: { active: true } });
    expect(live.find((i) => i.kind === 'save')).toBeTruthy();
  });

  it('maps the updater "available" state into an actionable item', () => {
    const { live, summary } = buildActivities({
      update: { state: { status: 'available', version: '1.2.3' } },
    });
    const item = live.find((i) => i.kind === 'update');
    expect(item!.status).toBe('available');
    expect(item!.actions!.map((a) => a.id)).toEqual(['update-rebuild', 'update-dismiss']);
    expect(summary.needsAction).toBe(true);
  });

  it('flags errors and floats them to the front of the list', () => {
    const { live, summary } = buildActivities({
      analysis: { jobs: [{ key: 'a', status: 'analyzing' }] },
      update: { state: { status: 'error', version: '9', error: 'boom' } },
    });
    expect(summary.hasError).toBe(true);
    expect(live[0].kind).toBe('update'); // error sorts first
    expect(live[0].status).toBe('error');
  });

  it('builds a compact "+N" headline when several activities run at once', () => {
    const downloadJobs = [{ key: '1', status: 'downloading', progress: 0 }];
    const { summary } = buildActivities({
      analysis: { jobs: [{ key: 'a', status: 'analyzing' }] },
      downloads: { jobs: downloadJobs },
    });
    expect(summary.activeCount).toBe(2);
    expect(summary.headline).toMatch(/ · \+1$/);
  });
});
