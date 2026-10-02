// The web client's LinksApi (src/api/links.ts) on `POST /api/v1/links`
// (contract C7). The route itself lands with P2-11; against today's server it
// answers 404 `not_found` (axum's default for an unmatched API path), which
// this client surfaces like any other failure — the /share page's error
// state (web/src/share/SharePage.tsx) already covers it. Not in the
// generated OpenAPI client yet (./schema.ts), so the request and response
// shapes are hand-typed here, exactly as contract C7 defines them; P2-11
// replaces them with the generated `Schemas['Link*']` types.
import type { LinkResult, LinksApi } from '@ui/api/links';
import type { Http } from './http';

interface LinkCreateRequest {
  url: string;
  note: string | null;
  tags: string[] | null;
}

interface LinkCreateResponse {
  key: string;
  platform: string;
  created: boolean;
}

export function createLinksApi(http: Http): LinksApi {
  return {
    async create(url, opts = {}): Promise<LinkResult> {
      const body: LinkCreateRequest = {
        url,
        note: opts.note ?? null,
        tags: opts.tags ?? null,
      };
      const res = await http.send('POST', '/api/v1/links', body);
      const data = (await res.json()) as LinkCreateResponse;
      return { key: data.key, platform: data.platform, created: data.created };
    },
  };
}
