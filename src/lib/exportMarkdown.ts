// Markdown export + link copying for post result lists (AI Tags / AI Search).

// ── Markdown escaping ─────────────────────────────────────────────────────────
// Scraped/imported captions and URLs are untrusted: a `]`/`[` in the text or a
// `)` in the URL would break the `[desc](url)` link syntax (or inject markup).
// Escape the link text and emit URLs as <…> angle-bracket autolinks (which only
// need `>` escaped).
export function mdEscapeText(s: unknown): string {
  return String(s).replace(/[\\[\]]/g, '\\$&');
}

export function mdAutolink(url: unknown): string {
  return `<${String(url).replace(/>/g, '%3E')}>`;
}

// Result of copyPostLinks: each caller maps the outcome onto its own localized
// toast messages.
export interface CopyLinksResult {
  status: 'empty' | 'copied' | 'failed';
  n: number;
}

// One bullet per post: `- [desc](<url>) #tag1 #tag2`, with the description
// collapsed to a single line. Posts without a permalink render as plain text.
export function postsToMarkdown(posts: Shelfy.Post[]): string {
  return posts
    .map((p) => {
      const desc = mdEscapeText(
        (p.aiDescription || p.text || p.authorUsername || 'post').replace(/\s+/g, ' ').trim(),
      );
      const tags = (p.aiTags || []).map((t) => `#${t}`).join(' ');
      const base = p.postUrl ? `- [${desc}](${mdAutolink(p.postUrl)})` : `- ${desc}`;
      return tags ? `${base} ${tags}` : base;
    })
    .join('\n');
}

// Save the given markdown as a file via a transient download anchor.
export function downloadMarkdown(md: string, filename: string): void {
  const blob = new Blob([md], { type: 'text/markdown' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

// Copy the post permalinks (one per line) to the clipboard. Returns
// { status: 'empty' | 'copied' | 'failed', n } so each caller can map the
// outcome onto its own localized toast messages.
export async function copyPostLinks(posts: Shelfy.Post[]): Promise<CopyLinksResult> {
  const links = posts.map((p) => p.postUrl).filter((u): u is string => Boolean(u));
  if (links.length === 0) return { status: 'empty', n: 0 };
  try {
    await navigator.clipboard.writeText(links.join('\n'));
    return { status: 'copied', n: links.length };
  } catch {
    return { status: 'failed', n: links.length };
  }
}
