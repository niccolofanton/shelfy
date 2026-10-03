import { MSG, PLATFORMS, type ExternalAnswer } from '../../shared/protocol';
import type { PlannerJob } from '../../sw/planner/service';
import type { ScheduleSettings } from '../../sw/planner/model';
import { button, el, setText } from '../dom';
import type { PanelSection } from '../section';

export const plannerSection: PanelSection = {
  id: 'planner',
  mount(root, ctx) {
    const t = ctx.t;
    const status = el('p', {
      className: 'status',
      attrs: { role: 'status', 'data-testid': 'planner-status' },
    });
    const rows = el('div', { attrs: { 'data-testid': 'planner-jobs' } });
    const enabled = el('input', {
      attrs: { type: 'checkbox', 'data-testid': 'planner-reminder-enabled' },
    });
    const unattended = el('input', {
      attrs: { type: 'checkbox', 'data-testid': 'planner-unattended' },
    });
    const time = el('input', {
      attrs: { type: 'time', value: '18:00', 'data-testid': 'planner-hour' },
    });
    const enableLabel = el('label', { className: 'toggle' });
    enableLabel.append(enabled, el('span', { text: t('planner.reminder') }));
    const unattendedLabel = el('label', { className: 'toggle' });
    unattendedLabel.append(unattended, el('span', { text: t('planner.unattended') }));
    const timeLabel = el('label', { className: 'field' });
    timeLabel.append(el('span', { text: t('planner.hour') }), time);
    let dirty = false;
    let busy = false;
    for (const field of [enabled, unattended, time])
      field.addEventListener('change', () => {
        dirty = true;
        unattended.disabled = !enabled.checked;
        time.disabled = !enabled.checked;
      });
    const start = button(
      t('planner.syncAll'),
      () => {
        if (busy) return;
        busy = true;
        start.disabled = true;
        setText(status, t('planner.starting'));
        ctx
          .send<{ ok: boolean; results?: ExternalAnswer[] }>({ kind: MSG.plannerStartAll })
          .then((answer) => {
            const failures =
              answer.results
                ?.filter((result) => !result.ok)
                .map((result) => ('code' in result ? result.code : '')) ?? [];
            setText(
              status,
              failures.length ? failures.map((code) => ctx.errorText(code)).join(' · ') : '',
            );
          })
          .catch(() => setText(status, t('planner.requestFailed')))
          .finally(() => {
            busy = false;
            ctx.refresh();
          });
      },
      { testId: 'planner-start-all' },
    );
    const save = button(
      t('planner.save'),
      () => {
        const [hour, minute] = time.value.split(':').map(Number);
        if (!Number.isInteger(hour) || !Number.isInteger(minute)) {
          setText(status, t('planner.invalidTime'));
          return;
        }
        save.disabled = true;
        ctx
          .send<ExternalAnswer>({
            kind: MSG.plannerSchedule,
            schedule: { enabled: enabled.checked, hour, minute, unattended: unattended.checked },
          })
          .then((answer) => {
            if (answer.ok) {
              dirty = false;
              setText(status, t('planner.saved'));
            } else setText(status, ctx.errorText(answer.code));
          })
          .catch(() => setText(status, t('planner.requestFailed')))
          .finally(() => {
            save.disabled = false;
            ctx.refresh();
          });
      },
      { testId: 'planner-save-schedule' },
    );
    root.append(
      el('h2', { text: t('planner.title') }),
      el('p', { className: 'muted', text: t('planner.hint') }),
      start,
      rows,
      enableLabel,
      timeLabel,
      unattendedLabel,
      el('p', { className: 'muted', text: t('planner.unattendedHint') }),
      save,
      status,
    );
    let request = 0;
    return {
      update(state) {
        start.disabled =
          busy ||
          !state.paired ||
          state.outdated ||
          !PLATFORMS.some(
            (platform) => state.serverModes[platform].scroll || state.serverModes[platform].replay,
          );
        const mine = ++request;
        ctx
          .send<{ jobs: PlannerJob[]; schedule: ScheduleSettings }>({ kind: MSG.plannerGet })
          .then((snapshot) => {
            if (mine !== request) return;
            if (!dirty) {
              enabled.checked = snapshot.schedule.enabled;
              unattended.checked = snapshot.schedule.unattended;
              time.value = `${String(snapshot.schedule.hour).padStart(2, '0')}:${String(snapshot.schedule.minute).padStart(2, '0')}`;
            }
            unattended.disabled = !enabled.checked;
            time.disabled = !enabled.checked;
            rows.replaceChildren();
            for (const job of snapshot.jobs) {
              const row = el('div', { className: 'card' });
              row.append(
                el('p', {
                  text: t('planner.progress', {
                    platform: t(`platform.${job.platform}`),
                    status: t(`planner.status.${job.status}`),
                    step: job.step,
                    total: job.total,
                    skipped: job.skipped,
                  }),
                  attrs: { 'data-testid': `planner-job-${job.platform}` },
                }),
              );
              if (job.code)
                row.append(
                  el('p', {
                    className: 'muted',
                    text:
                      t(`planner.error.${job.code}`) === `planner.error.${job.code}`
                        ? ctx.errorText(job.code)
                        : t(`planner.error.${job.code}`),
                  }),
                );
              if (job.status === 'navigating' || job.status === 'syncing')
                row.append(
                  button(
                    t('planner.stop'),
                    () => {
                      void ctx
                        .send({ kind: MSG.plannerStop, platform: job.platform })
                        .catch(() => setText(status, t('planner.requestFailed')))
                        .finally(ctx.refresh);
                    },
                    { className: 'danger', testId: `planner-stop-${job.platform}` },
                  ),
                );
              rows.append(row);
            }
          })
          .catch(() => setText(status, t('planner.requestFailed')));
      },
    };
  },
};
