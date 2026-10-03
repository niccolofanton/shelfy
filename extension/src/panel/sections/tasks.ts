import { MSG } from '../../shared/protocol';
import type { TasksState } from '../../sw/tasks/service';
import { button, el, setText } from '../dom';
import type { PanelSection } from '../section';

export const tasksSection: PanelSection = {
  id: 'tasks',
  mount(root, ctx) {
    const waiting = el('p', { attrs: { 'data-testid': 'tasks-waiting', role: 'status' } });
    const detail = el('p', { className: 'muted' });
    let running = false;
    const poll = button(
      ctx.t('tasks.check'),
      () => {
        if (running) return;
        running = true;
        poll.disabled = true;
        void ctx
          .send({ kind: MSG.tasksPoll })
          .catch(() => setText(detail, ctx.t('tasks.requestFailed')))
          .finally(() => {
            running = false;
            ctx.refresh();
          });
      },
      { testId: 'tasks-poll' },
    );
    root.append(el('h2', { text: ctx.t('tasks.title') }), waiting, detail, poll);
    let request = 0;
    return {
      update(state) {
        const mine = ++request;
        poll.disabled = running || !state.paired || state.outdated;
        void ctx
          .send<TasksState>({ kind: MSG.tasksGet })
          .then((tasks) => {
            if (mine !== request) return;
            const total = tasks.waiting.instagram + tasks.waiting.twitter + tasks.waiting.pinterest;
            setText(
              waiting,
              ctx.t(tasks.waiting.instagram ? 'tasks.waitingInstagram' : 'tasks.waiting', {
                count: total,
              }),
            );
            setText(
              detail,
              tasks.code
                ? ctx.t(`tasks.code.${tasks.code}`) === `tasks.code.${tasks.code}`
                  ? ctx.errorText(tasks.code)
                  : ctx.t(`tasks.code.${tasks.code}`)
                : tasks.running
                  ? ctx.t('tasks.running')
                  : ctx.t('tasks.hint'),
            );
            poll.disabled ||= tasks.running;
          })
          .catch(() => setText(detail, ctx.t('tasks.requestFailed')));
      },
    };
  },
};
