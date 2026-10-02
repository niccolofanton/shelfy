// Where to find the shared link in a Web Share Target request (plan §2.17
// Android; P2-07 acceptance 3): `GET /share?url=&text=&title=`. Android's
// share sheet commonly puts the page's URL inside `text` — sometimes with a
// caption first ("Check this out: https://…") — rather than in `url` alone;
// the bookmarklet always uses `url`. iOS has no Web Share Target, but the
// Shortcut and the bookmarklet open this same page, so the same extraction
// covers them too.

export interface SharedFields {
  url?: string | null;
  text?: string | null;
  title?: string | null;
}

// A bare http(s) URL token. Excludes the closing punctuation a share sheet
// often appends after the link (a sentence's full stop, a wrapping bracket);
// TRAILING_PUNCTUATION below cleans up what still sneaks in at the end.
const URL_PATTERN = /https?:\/\/[^\s<>"'()[\]{}]+/i;
const TRAILING_PUNCTUATION = /[.,;:!?]+$/;

function firstUrlIn(value: string | null | undefined): string | null {
  if (!value) return null;
  const match = URL_PATTERN.exec(value);
  if (!match) return null;
  const candidate = match[0].replace(TRAILING_PUNCTUATION, '');
  try {
    const parsed = new URL(candidate);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? candidate : null;
  } catch {
    return null;
  }
}

// The first http(s) URL among `url`, `text` and `title`, in that order; null
// when none of them has one.
export function extractSharedUrl({ url, text, title }: SharedFields): string | null {
  return firstUrlIn(url) ?? firstUrlIn(text) ?? firstUrlIn(title) ?? null;
}
