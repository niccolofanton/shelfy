import { createTranslate, pickLang } from '../../shared/i18n';
import type { ScheduleSettings } from './model';
import { nextReminder } from './model';
import { PLANNER_ALARM, REMINDER_NOTIFICATION, type PlannerService } from './service';

// P2-17 registers its worker here. No task implementation or pretend polling
// exists in P2-15: both the 5-minute alarm and completion of a planner sync
// call exactly this seam once a consumer has registered it.
let taskPollHandler: (() => Promise<void>) | null = null;
export function registerTaskPollHandler(handler: () => Promise<void>): () => void {
  taskPollHandler = handler;
  return () => {
    if (taskPollHandler === handler) taskPollHandler = null;
  };
}
export async function pollTasks(): Promise<void> {
  await taskPollHandler?.();
}
export interface SchedulerDeps {
  planner: Pick<PlannerService, 'schedule' | 'setSchedule' | 'startAll'>;
  alarms: {
    get(name: string): Promise<unknown>;
    create(name: string, info: { when?: number; periodInMinutes?: number }): Promise<void>;
    clear(name: string): Promise<boolean>;
  };
  notify(id: string, options: { title: string; message: string; button: string }): Promise<void>;
  clearNotification(id: string): Promise<void>;
  now(): number;
  languages?: readonly string[];
}
export class PlannerScheduler {
  constructor(private readonly deps: SchedulerDeps) {}
  async ensure(): Promise<void> {
    if (!(await this.deps.alarms.get(PLANNER_ALARM.taskPoll)))
      await this.deps.alarms.create(PLANNER_ALARM.taskPoll, { periodInMinutes: 5 });
    await this.scheduleReminder(await this.deps.planner.schedule());
  }
  private async scheduleReminder(schedule: ScheduleSettings): Promise<void> {
    if (!schedule.enabled) {
      await this.deps.alarms.clear(PLANNER_ALARM.reminder);
      return;
    }
    await this.deps.alarms.create(PLANNER_ALARM.reminder, {
      when: nextReminder(this.deps.now(), schedule),
    });
  }
  async setSchedule(schedule: ScheduleSettings): Promise<void> {
    await this.deps.planner.setSchedule(schedule);
    await this.scheduleReminder(schedule);
  }
  async alarm(name: string): Promise<void> {
    if (name === PLANNER_ALARM.taskPoll) {
      await pollTasks();
      return;
    }
    if (name !== PLANNER_ALARM.reminder) return;
    const schedule = await this.deps.planner.schedule();
    if (!schedule.enabled) return;
    await this.scheduleReminder(schedule);
    if (schedule.unattended) {
      await this.deps.planner.startAll('scheduled', true);
      return;
    }
    const t = createTranslate(pickLang(this.deps.languages));
    await this.deps.notify(REMINDER_NOTIFICATION, {
      title: t('planner.reminderTitle'),
      message: t('planner.reminderMessage'),
      button: t('planner.syncNow'),
    });
  }
  async clicked(id: string): Promise<void> {
    if (id !== REMINDER_NOTIFICATION) return;
    await this.deps.clearNotification(id);
    await this.deps.planner.startAll('web');
  }
}
