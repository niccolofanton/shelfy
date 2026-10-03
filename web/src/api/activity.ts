import type { ActivityApi, NotificationPage } from '@ui/api/activity';
import type { EventStream } from './events';
import type { Http } from './http';

export function createActivityApi(http: Http, events: Pick<EventStream, 'on'>): ActivityApi {
  return {
    list({ limit, cursor }) {
      const query = new URLSearchParams({ limit: String(Math.max(1, Math.min(200, limit))) });
      if (cursor) query.set('cursor', cursor);
      return http.get<NotificationPage>('/api/v1/notifications', query);
    },
    async read(selector) {
      const response = await http.send('POST', '/api/v1/notifications/read', selector);
      return response.json();
    },
    onNotification(listener) {
      return events.on('notification', listener);
    },
    onRefresh(listener) {
      const hello = events.on('hello', listener);
      const resync = events.on('resync', listener);
      return () => {
        hello();
        resync();
      };
    },
  };
}
