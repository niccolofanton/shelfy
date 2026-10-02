// Turning a URL into a post from outside the normal sync paths (web port plan
// §2.17 Mobile and the share sheet, §2.19 Routes `/share`; contract C7): the
// Android share target, the bookmarklet and the iOS Shortcut all end at
// `POST /links`. Only the web client has one (ShelfyClient.links): the
// desktop has no `/share` page, mirroring how ShelfyClient.account works.
//
// The operation is transport-neutral; the web client (web/src/api/links.ts)
// maps it onto `/api/v1/links`. Failures reject with the API's problem `code`
// (src/api/errors.ts).

export interface LinkResult {
  // The post's canonical key (`ig_…`, `x_…`, `pin_…`, `web_…`): where "Open"
  // goes (`/p/:key`).
  key: string;
  platform: string;
  // False when the URL already named a post in the library: nothing new was
  // created, but any note or tags passed were still recorded.
  created: boolean;
}

export interface LinksApi {
  // Turns `url` into a post (C7). A link already saved stays the same post
  // (`created: false`); its note and tags unite with what is passed here.
  create(url: string, opts?: { note?: string | null; tags?: string[] | null }): Promise<LinkResult>;
}
