import { session } from 'electron';

// No `persist:` prefix: this cookie jar exists only in memory and is separate
// from the logged-in social webview. `omit` also prevents cookies being sent or
// accepted by these requests.
export const ANONYMOUS_MEDIA_PARTITION = 'shelfy-anonymous-media';

export function fetchAnonymousMedia(url: string, init: RequestInit): Promise<Response> {
  const headers = new Headers(init.headers);
  headers.delete('cookie');
  headers.delete('authorization');
  headers.delete('proxy-authorization');
  return session.fromPartition(ANONYMOUS_MEDIA_PARTITION).fetch(url, {
    ...init,
    headers,
    credentials: 'omit',
  });
}
