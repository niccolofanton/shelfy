// The web client's LinksApi (src/api/links.ts) on `POST /api/v1/links`
// (contract C7, P2-11). The server answers 201 for a new post and 200 for a
// link saved before; failures reject with the problem `code`
// (`unsupported_link` for a URL Shelfy does not save, `unavailable` for a
// short link it cannot resolve now), which the /share page's error state
// (web/src/share/SharePage.tsx) shows.
import type { LinkResult, LinksApi } from '@ui/api/links';
import type { Http } from './http';
import type { components } from './schema';

type Schemas = components['schemas'];

export function createLinksApi(http: Http): LinksApi {
  return {
    async create(url, opts = {}): Promise<LinkResult> {
      const body: Schemas['LinkCreate'] = {
        url,
        note: opts.note ?? null,
        tags: opts.tags ?? null,
      };
      const res = await http.send('POST', '/api/v1/links', body);
      const data = (await res.json()) as Schemas['LinkCreated'];
      return { key: data.key, platform: data.platform, created: data.created };
    },
  };
}
