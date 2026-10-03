// Tags Explorer transports and review actions. Long-running jobs remain gated
// until their queue controls land; no desktop bridge is accessed here.
import type { AiTagsApi } from '@ui/api/ai';
import type { PostQuery } from '@ui/api/ShelfyClient';
import type { Http } from '../http';
import { listPostsParams, toPost, toSearchParams } from '../mapping';
import type { components } from '../schema';
type Schemas = components['schemas'];
const ROOT = '/api/v1';
const segment = encodeURIComponent;

export function createTagsApi(http: Http): AiTagsApi {
  async function write<T>(
    method: 'POST' | 'PATCH' | 'DELETE',
    path: string,
    body?: unknown,
  ): Promise<T> {
    return (await (await http.send(method, `${ROOT}${path}`, body)).json()) as T;
  }
  const unsupported = async (): Promise<never> => {
    throw new Error('AI job controls are unavailable');
  };
  return {
    getOverview: () => http.get<Schemas['TagOverview']>(`${ROOT}/tags/overview`),
    async getTagStats({ limit = 200, tier } = {}) {
      const { items } = await http.get<Schemas['TagStats']>(
        `${ROOT}/tags`,
        new URLSearchParams({ limit: String(limit), tier: tier ?? 'all' }),
      );
      return items.map((row) => ({
        ...row,
        lastUsed: row.lastUsed == null ? null : new Date(row.lastUsed).toISOString(),
      }));
    },
    async getEntityStats({ limit = 60 } = {}) {
      return (
        await http.get<Schemas['EntityStats']>(
          `${ROOT}/entities`,
          new URLSearchParams({ limit: String(limit) }),
        )
      ).items;
    },
    async getTagCooccurrence(tag, limit = 12) {
      return (
        await http.get<Schemas['RelatedTags']>(`${ROOT}/tags/${segment(tag)}/related`)
      ).items.slice(0, limit);
    },
    getHealth: () => http.get<Schemas['TagHealth']>(`${ROOT}/tags/health`),
    async getMergeSuggestions({ limit = 30 } = {}) {
      return (
        await http.get<Schemas['MergeSuggestions']>(
          `${ROOT}/tags/merge-suggestions`,
          new URLSearchParams({ limit: String(limit) }),
        )
      ).items;
    },
    renameTag: (from, to) => write('POST', '/tags/rename', { from, to }),
    mergeTags: (sources, target) => write('POST', '/tags/merge', { sources, target }),
    async getClusters({ maxClusters = 24 } = {}) {
      return (await http.get<Schemas['ClusterList']>(`${ROOT}/tag-clusters`)).items.slice(
        0,
        maxClusters,
      );
    },
    acceptCluster: (id) => write('PATCH', `/tag-clusters/${id}`, { status: 'accepted' }),
    dismissCluster: (id) => write('DELETE', `/tag-clusters/${id}`),
    renameCluster: (id, label) => write('PATCH', `/tag-clusters/${id}`, { label }),
    removeTagFromCluster: (tag, id) => write('DELETE', `/tag-clusters/${id}/tags/${segment(tag)}`),
    async getAliases({ status } = {}) {
      return (
        await http.get<Schemas['AliasList']>(
          `${ROOT}/tag-aliases`,
          status ? new URLSearchParams({ status }) : undefined,
        )
      ).items;
    },
    async acceptAlias(alias) {
      const result = await write<Schemas['AliasesAccepted']>(
        'POST',
        `/tag-aliases/${segment(alias)}/accept`,
      );
      return { ok: true, rewritten: result.rewritten };
    },
    acceptAllAliases: () => write('POST', '/tag-aliases/accept-all'),
    async dismissAlias(alias) {
      const result = await write<Schemas['AliasDismissed']>(
        'POST',
        `/tag-aliases/${segment(alias)}/dismiss`,
      );
      return { ok: result.ok, rewritten: 0 };
    },
    async getPostIdsByTags(tags, mode = 'or') {
      const result = await write<Schemas['TagPostKeys']>('POST', '/tags/post-keys', { tags, mode });
      if (result.truncated) throw new Error('Tag selection exceeds the post-key limit');
      return result.keys;
    },
    async getPosts(filters) {
      const query = (filters ?? {}) as PostQuery & { limit?: number; offset?: number };
      if (query.offset) throw new Error('Use cursor pagination for tag results');
      const posts: Shelfy.Post[] = [];
      const limit = Math.max(1, Math.min(1000, query.limit ?? 200));
      let cursor: string | null = null;
      let total = 0;
      do {
        const page: Schemas['PostPage'] = await http.get(
          `${ROOT}/posts`,
          toSearchParams(
            listPostsParams(query, { limit: limit - posts.length, cursor, includeTotal: !cursor }),
          ),
        );
        posts.push(...page.items.map(toPost));
        total = page.total ?? total;
        cursor = page.nextCursor;
      } while (cursor && posts.length < limit);
      return { posts, total };
    },
    regenerateClusters: unsupported,
    cancelClusters: unsupported,
    proposeAliases: unsupported,
    cancelAliases: unsupported,
  };
}
