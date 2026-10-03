import type { LibraryFacetsApi } from '@ui/api/facets';
import type { EventStream } from './events';
import type { Http } from './http';
import type { components } from './schema';

export function createFacetsApi(http: Http, events: Pick<EventStream, 'on'>): LibraryFacetsApi {
  return {
    get: (signal) => http.get<components['schemas']['Facets']>('/api/v1/facets', undefined, signal),
    onChanged(listener) {
      const offChanged = events.on('posts.changed', listener);
      const offResync = events.on('resync', listener);
      return () => {
        offChanged();
        offResync();
      };
    },
  };
}
