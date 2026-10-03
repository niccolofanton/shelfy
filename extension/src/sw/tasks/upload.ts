import {
  ACCESS_ID_HEADER,
  ACCESS_SECRET_HEADER,
  EXTENSION_HEADER,
  type ApiCredentials,
} from '../api';
import type { ExtensionTask } from './contracts';
import { TaskError } from './contracts';

export const MAX_IMAGE_BYTES = 15 * 1024 * 1024;
const CHUNK_BYTES = 1024 * 1024;
const TYPES: Record<string, string> = {
  'image/jpeg': 'jpg',
  'image/png': 'png',
  'image/webp': 'webp',
  'image/gif': 'gif',
  'image/avif': 'avif',
};
export function allowedCdn(task: Pick<ExtensionTask, 'platform' | 'url'>): string | null {
  try {
    const url = new URL(task.url ?? '');
    if (
      url.protocol !== 'https:' ||
      url.username ||
      url.password ||
      url.port ||
      /\.(?:mp4|m3u8|webm|mov)(?:$|\/)/i.test(url.pathname)
    )
      return null;
    const host = url.hostname;
    const allowed =
      task.platform === 'instagram'
        ? /(?:^|\.)(?:cdninstagram\.com|fbcdn\.net)$/.test(host)
        : task.platform === 'twitter'
          ? host === 'pbs.twimg.com'
          : /(?:^|\.)pinimg\.com$/.test(host);
    return allowed ? url.href : null;
  } catch {
    return null;
  }
}
export function imageExtension(type: string, bytes: Uint8Array): string | null {
  const ext = TYPES[type.toLowerCase().split(';')[0].trim()];
  const ascii = (start: number, end: number) => String.fromCharCode(...bytes.slice(start, end));
  const matches =
    ext === 'jpg'
      ? bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255
      : ext === 'png'
        ? ascii(1, 4) === 'PNG' &&
          bytes[0] === 137 &&
          bytes[4] === 13 &&
          bytes[5] === 10 &&
          bytes[6] === 26 &&
          bytes[7] === 10
        : ext === 'webp'
          ? ascii(0, 4) === 'RIFF' && ascii(8, 12) === 'WEBP'
          : ext === 'gif'
            ? ['GIF87a', 'GIF89a'].includes(ascii(0, 6))
            : ext === 'avif'
              ? ascii(4, 8) === 'ftyp' && ['avif', 'avis'].includes(ascii(8, 12))
              : false;
  return matches ? ext : null;
}
export async function readImage(response: Response): Promise<{ bytes: Uint8Array; ext: string }> {
  if (!response.ok) throw new TaskError(`cdn_http_${response.status}`);
  const type = response.headers.get('content-type') ?? '';
  if (!TYPES[type.toLowerCase().split(';')[0].trim()]) {
    await response.body?.cancel();
    throw new TaskError('image_type');
  }
  const length = response.headers.get('content-length');
  if (length && Number(length) > MAX_IMAGE_BYTES) {
    await response.body?.cancel();
    throw new TaskError('image_too_large');
  }
  const reader = response.body?.getReader();
  if (!reader) throw new TaskError('image_empty');
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_IMAGE_BYTES) {
      await reader.cancel();
      throw new TaskError('image_too_large');
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  const ext = imageExtension(type, bytes);
  if (!ext) throw new TaskError('image_type');
  return { bytes, ext };
}
export interface ImageUploadDeps {
  origin: string;
  version: string;
  credentials(): Promise<ApiCredentials>;
  fetch(input: string, init: RequestInit): Promise<Response>;
  guard(): Promise<void>;
}
/** Image bytes only. CDN requests have neither cookies nor Shelfy credentials;
 * tus requests are pinned to the polled token and Shelfy's own origin. */
export async function uploadImage(
  task: ExtensionTask,
  deps: ImageUploadDeps,
  signal?: AbortSignal,
): Promise<string> {
  const url = allowedCdn(task);
  if (!url) throw new TaskError('cdn_not_allowed');
  await deps.guard();
  const controller = new AbortController();
  const cancel = () => controller.abort();
  signal?.addEventListener('abort', cancel, { once: true });
  if (signal?.aborted) cancel();
  const timer = setTimeout(cancel, 45_000);
  try {
    const response = await deps.fetch(url, {
      credentials: 'omit',
      redirect: 'manual',
      cache: 'no-store',
      signal: controller.signal,
    });
    const { bytes, ext } = await readImage(response);
    await deps.guard();
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes.slice().buffer));
    const sha256 = [...hash].map((byte) => byte.toString(16).padStart(2, '0')).join('');
    const credentials = await deps.credentials();
    if (!credentials.token) throw new TaskError('cancelled');
    const headers: Record<string, string> = {
      'Tus-Resumable': '1.0.0',
      [EXTENSION_HEADER]: deps.version,
      Authorization: `Bearer ${credentials.token}`,
    };
    if (credentials.access) {
      headers[ACCESS_ID_HEADER] = credentials.access.clientId;
      headers[ACCESS_SECRET_HEADER] = credentials.access.clientSecret;
    }
    const request = async (path: string, init: RequestInit): Promise<Response> => {
      await deps.guard();
      const target = new URL(path, deps.origin);
      if (target.origin !== deps.origin) throw new TaskError('upload_location');
      return deps.fetch(target.href, {
        ...init,
        credentials: 'include',
        redirect: 'manual',
        signal: controller.signal,
      });
    };
    const created = await request('/api/v1/uploads', {
      method: 'POST',
      headers: {
        ...headers,
        'Upload-Length': String(bytes.byteLength),
        'Upload-Metadata': `purpose ${btoa('archive-object')},sha256 ${btoa(sha256)},ext ${btoa(ext)}`,
      },
    });
    if (created.status !== 201) throw new TaskError(`upload_http_${created.status}`);
    const location = new URL(created.headers.get('location') ?? '', deps.origin);
    const match = /^\/api\/v1\/uploads\/([A-Za-z0-9_-]{1,128})$/.exec(location.pathname);
    if (
      location.origin !== deps.origin ||
      !match ||
      location.search ||
      location.hash ||
      location.username ||
      location.password
    )
      throw new TaskError('upload_location');
    let offset = 0;
    while (offset < bytes.byteLength) {
      const end = Math.min(offset + CHUNK_BYTES, bytes.byteLength);
      let patched: Response | null = null;
      try {
        patched = await request(location.href, {
          method: 'PATCH',
          headers: {
            ...headers,
            'Upload-Offset': String(offset),
            'Content-Type': 'application/offset+octet-stream',
          },
          body: bytes.slice(offset, end),
        });
      } catch (error) {
        if (controller.signal.aborted || error instanceof TaskError) throw error;
      }
      if (patched && patched.status !== 204 && patched.status !== 409)
        throw new TaskError(`upload_http_${patched.status}`);
      const answer =
        patched?.status === 204
          ? patched
          : await request(location.href, { method: 'HEAD', headers });
      if (answer.status !== 204 && answer.status !== 200)
        throw new TaskError(`upload_http_${answer.status}`);
      const rawOffset = answer.headers.get('upload-offset');
      const next = rawOffset && /^\d+$/.test(rawOffset) ? Number(rawOffset) : NaN;
      if (!Number.isSafeInteger(next) || next <= offset || next > end)
        throw new TaskError('upload_offset');
      offset = next;
    }
    await deps.guard();
    return match[1];
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', cancel);
  }
}
