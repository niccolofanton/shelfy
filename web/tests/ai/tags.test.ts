import { describe, it, expect, vi } from 'vitest';
import { createTagsApi } from '../../src/api/ai/tags';
import type { Http } from '../../src/api/http';
import { ApiError } from '../../src/api/http';
import { apiPost } from '../fixtures';
import { toPostSelector } from '../../src/api/mapping';

function transport() {
  const get = vi.fn();
  const send = vi.fn(
    async () => new Response(JSON.stringify({ updated: 1, accepted: 2, rewritten: 3, ok: true })),
  );
  return { get, send, api: createTagsApi({ get, send } as unknown as Http) };
}
describe('web tags transport', () => {
  it('maps epoch times and requests the chosen tier', async () => {
    const { get, api } = transport();
    get.mockResolvedValue({ items: [{ tag: 'Lamp', count: 2, lastUsed: 0, categories: [] }] });
    expect(await api.getTagStats({ tier: 'manual' })).toEqual([
      { tag: 'Lamp', count: 2, lastUsed: '1970-01-01T00:00:00.000Z', categories: [] },
    ]);
    expect(get.mock.calls[0][1].get('tier')).toBe('manual');
  });
  it('encodes tag path segments and maps review responses', async () => {
    const { send, api } = transport();
    expect(await api.acceptAlias('a/b')).toEqual({ ok: true, rewritten: 3 });
    expect(send).toHaveBeenLastCalledWith('POST', '/api/v1/tag-aliases/a%2Fb/accept', undefined);
    expect(await api.dismissAlias('a/b')).toEqual({
      ok: true,
      rewritten: 0,
    });
    await api.removeTagFromCluster('a/b', 7);
    expect(send).toHaveBeenLastCalledWith('DELETE', '/api/v1/tag-clusters/7/tags/a%2Fb', undefined);
  });
  it('accepts every alias in one atomic backend request', async () => {
    const { send, api } = transport();
    await api.acceptAllAliases?.();
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0]).toEqual(['POST', '/api/v1/tag-aliases/accept-all', undefined]);
  });
  it('paginates tag AND/entity results and preserves the total', async () => {
    const { get, api } = transport();
    get
      .mockResolvedValueOnce({ items: [apiPost()], total: 301, nextCursor: 'next' })
      .mockResolvedValueOnce({ items: [apiPost({ key: 'ig_2' })], total: null, nextCursor: null });
    const result = await api.getPosts({
      tags: ['art', 'lamp'],
      tagMode: 'and',
      entity: 'Studio',
      limit: 300,
    });
    expect(result.total).toBe(301);
    expect(result.posts.map((post) => post.id)).toEqual(['ig_1001', 'ig_2']);
    expect(get.mock.calls[0][1].getAll('tags')).toEqual(['art', 'lamp']);
    expect(get.mock.calls[0][1].get('tagMode')).toBe('and');
    expect(get.mock.calls[1][1].get('cursor')).toBe('next');
    expect(get.mock.calls[1][1].get('entity')).toBe('Studio');
  });
  it('refuses truncated explicit keys; bulk filter remains complete', async () => {
    const { send, api } = transport();
    send.mockResolvedValue(new Response(JSON.stringify({ keys: ['ig_1'], truncated: true })));
    await expect(api.getPostIdsByTags(['art'])).rejects.toThrow('limit');
    expect(toPostSelector({ filter: { tags: ['art', 'lamp'], tagMode: 'and' } })).toEqual({
      filter: { tags: ['art', 'lamp'], tagMode: 'and' },
    });
    expect(toPostSelector({ filter: { entity: 'Studio' } })).toEqual({
      filter: { entity: 'Studio' },
    });
  });
  it('maps merge/rename and every cluster review mutation to its method and payload', async () => {
    const { send, api } = transport();
    await api.mergeTags(['one', 'two'], 'three');
    expect(send).toHaveBeenLastCalledWith('POST', '/api/v1/tags/merge', {
      sources: ['one', 'two'],
      target: 'three',
    });
    await api.renameTag('three', 'Four');
    expect(send).toHaveBeenLastCalledWith('POST', '/api/v1/tags/rename', {
      from: 'three',
      to: 'Four',
    });
    await api.acceptCluster(99);
    expect(send).toHaveBeenLastCalledWith('PATCH', '/api/v1/tag-clusters/99', {
      status: 'accepted',
    });
    await api.renameCluster(99, 'Lighting');
    expect(send).toHaveBeenLastCalledWith('PATCH', '/api/v1/tag-clusters/99', {
      label: 'Lighting',
    });
    await api.dismissCluster(99);
    expect(send).toHaveBeenLastCalledWith('DELETE', '/api/v1/tag-clusters/99', undefined);
  });
  it('preserves stable API errors for localized UI handling', async () => {
    const { get, api } = transport();
    const error = new ApiError(401, 'unauthorized');
    get.mockRejectedValue(error);
    await expect(api.getOverview()).rejects.toBe(error);
  });
});
