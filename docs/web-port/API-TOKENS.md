# Library API tokens (F21)

Desktop API clients (X5) and MCP integrations (X6) use token kind `library`.
Tokens act as the account that created them, with the same per-user isolation
as a browser session. Existing extension, Shortcut and migration token scopes
are unchanged.

Create a token with a signed-in browser session and a sign-in or re-authentication
from the last five minutes:

```http
POST /api/v1/me/tokens
Content-Type: application/json
X-Shelfy-Client: web
Origin: <the server's public URL>
Cookie: <the session cookie>

{"kind":"library","label":"My desktop","scopes":["library:read","library:write"]}
```

Omitting `scopes` grants **`library:read` only**. Explicit scopes must be a
non-empty subset of `library:read` and `library:write`; duplicates are stored
once. A write token does **not** imply read: request both for a client that
needs both. Mutation responses may return the resource just edited, added or
restored, including a pre-existing post returned by `POST /links`.

The response shows the `shx_…` token once. The database stores only its SHA-256
hash. Use `Authorization: Bearer shx_…` on permitted requests. A bearer request
does not require browser CSRF headers and never falls back to cookies, even
when the request carries a valid session. Missing/invalid credentials answer
401; a valid token lacking a required scope answers 403.

| Scope | Permitted routes |
| --- | --- |
| `library:read` | `GET /api/v1/posts`, `/posts/{key}`, `/posts/count`, `/search`, `/stats`, `/collections`, `/trash`; `POST /api/v1/posts/batch-get` and `/posts/lookup`; `GET,HEAD /media/{file}` |
| `library:write` | `PATCH /api/v1/posts/{key}` (notes, tags and manual AI edits); `POST /api/v1/posts/bulk`; collection create/edit/delete and membership writes; `POST /api/v1/collections/from-query`, `/trash/restore`, `/trash/empty`, `/links` |

`GET` routes also serve `HEAD` with the same policy. Read-only POSTs need
`library:read`. Existing `lookup` and `links:create` tokens still reach their
respective narrow routes. Tag reading/filtering is part of posts and search;
tag editing is part of post patch and bulk operations. There is no standalone
tag CRUD endpoint among the library-token routes. Alias and cluster review
endpoints retain their cookie-only policy.

Account settings, provider credentials, token/session management, passkeys,
re-authentication, jobs/queues and other cookie-only routes remain unavailable
to these tokens. Uploads, ingest, extension tasks, migrations and any future AI
routes retain their own access policy; a library scope grants no implicit
rights to a newly added route. Administrative commands have no library-token
entry point.

Another user's post, collection or media file is a 404, including when a
client knows its key, id or content digest. Batch and lookup reads omit unknown
keys. Collection ids are local to each user's library: an id that also exists
in the caller's library addresses the caller's collection.

List and revoke tokens with the existing cookie-only `GET /api/v1/me/tokens`
and `DELETE /api/v1/me/tokens/{id}`. Revocation takes effect on the next request;
the account list includes last use and expiry. This change leaves the existing
expiry policy unchanged (F9 owns TTL).

The authoritative route policy is `crates/server/src/routes/mod.rs`
`TOKEN_ROUTES`. Its OpenAPI security entries and the generated TypeScript
client are checked in, and `tests/library_tokens.rs` independently pins these
library rights. Media remains outside the OpenAPI document; the authz test
pins its GET/HEAD policy separately. Control migration `0006_library_tokens.sql`
widens the kind constraint at control schema v6 while preserving stored tokens
and the export metadata introduced by `0005_exports.sql` at v5. Both versions
have committed fixtures, and upgrades from v4 and v5 are checked.
