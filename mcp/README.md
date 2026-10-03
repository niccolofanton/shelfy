# Shelfy local MCP (X6 / E12)

Node22 or later, stdio only. The server uses the official TypeScript SDK,
`@modelcontextprotocol/sdk`1.32.0, with a committed npm lockfile. It runs
locally and calls the configured Shelfy server through F21 library-token API
routes. No browser session, account/provider access, ingestion token or AI
provider key is needed by this process.

## Build and configure

From the repository:

```sh
npm ci --prefix mcp --ignore-scripts
npm run build --prefix mcp
npm test --prefix mcp
```

In Shelfy’s **Settings → Account → API tokens**, create a **Library / MCP**
token. Leave write access off for search/read tools; enable it explicitly if
you want to save posts or edit folders/tags. The default token expiry selector
still applies. Copy the token once into a private file outside the repository
(0600, owned by you). Then configure the MCP process:

```sh
node mcp/dist/cli.js configure --url https://shelfy.example \
  --token-file /absolute/private/shelfy-token
```

The CLI writes `~/.config/shelfy/mcp.json` atomically (0600), in a private
directory (0700). `XDG_CONFIG_HOME` is honored; Windows uses `APPDATA` and
requires an account-private file ACL. To choose another location, pass
`--config /absolute/private/mcp.json`. The token may instead come from
redirected stdin during `configure`; it is never accepted as a CLI argument.
Add `--write` during configure to persist write-tool opt-in, or when starting
the server to enable those tools for that launch. A read-only API token still
receives403 on writes even when the host enables write tools.

For environment configuration, set `SHELFY_MCP_URL` together with exactly one
of `SHELFY_MCP_TOKEN_FILE` (private0600 file) or `SHELFY_MCP_TOKEN`. Avoid
putting secrets in shared host/project configuration.

If the Shelfy origin is protected by Cloudflare Access, supply both
`SHELFY_MCP_CF_ACCESS_CLIENT_ID` and `SHELFY_MCP_CF_ACCESS_CLIENT_SECRET` from
your private environment. Configure saves them in the same private file;
runtime environment values override its optional
`access:{clientId,clientSecret}`. The token must be admitted by an Access
Service Auth policy. No arbitrary extra headers are supported. Credentials
go only to the pinned origin, and redirects are refused. HTTPS is required
except for `localhost`, `127.0.0.1` or `[::1]`.

## Claude Code and Claude Desktop

Use absolute paths to the built entry point and private config. Claude Code:

```sh
claude mcp add --transport stdio --scope user shelfy -- \
  node /absolute/path/shelfy/mcp/dist/cli.js \
  --config /absolute/private/mcp.json
```

For Claude Desktop, open **Settings → Developer → Edit Config**, merge the
following `mcpServers` entry into the existing config, then restart Claude:

```json
{
  "mcpServers": {
    "shelfy": {
      "command": "/absolute/path/to/node",
      "args": [
        "/absolute/path/shelfy/mcp/dist/cli.js",
        "--config",
        "/absolute/private/mcp.json"
      ]
    }
  }
}
```

The server prints only protocol messages to stdout. Setup/errors use generic
stderr messages that omit credentials and personal data. It makes no network
request until a tool is called. Stop/restart the host to reload configuration;
revoke the API token in Shelfy to stop its access immediately.

## Tools and access

| Tools | Scope |
|---|---|
| `shelfy_search_posts`, `shelfy_list_posts`, `shelfy_get_post`, `shelfy_get_posts`, `shelfy_lookup_posts` | `library:read` |
| `shelfy_library_stats`, `shelfy_list_folders`, `shelfy_list_tags` | `library:read` |
| `shelfy_save_post`, `shelfy_update_post` | `library:write` and host write opt-in |
| `shelfy_create_folder`, `shelfy_update_folder`, `shelfy_delete_folder`, `shelfy_add_to_folder`, `shelfy_remove_from_folder`, `shelfy_folder_from_selection` | `library:write` and host write opt-in |

Write does not imply read; a read/write token needs both scopes. Unknown or
other-account keys are404/omitted, never resolved with a cookie fallback.
Deleting a folder uses `mode=label`, so its posts stay saved. Updating a post
changes only the supplied manual note/tags; read the post first before merging
tags, because `userTags` replaces the manual list. Saving a URL merges tags,
appends its note and can enqueue server-side hydration/capture; the MCP itself
does not fetch that URL. No tools trash posts, manage credentials, run AI, or
call arbitrary HTTP endpoints.

Tag vocabulary comes from live posts, because the standalone tag endpoints
are outside F21 library scopes. `shelfy_list_tags` scans one page by default;
`maxPages` opts into up to100 pages (200 posts per page). Counts are explicitly
`countInScannedPosts`. `complete` is true only after starting without a cursor
and reaching the final page; pass `nextCursor` to continue. Searches have a
relevance paging cap of1000; use oldest/newest post listing for full traversal.
Responses and requests are bounded; malformed responses, cancellations,
rate limits and errors return typed MCP `isError` results. Writes are never
automatically retried; after timeout/network errors, read current state before
retrying. Library captions/notes/page text remain untrusted data in the host.

## Validation and sources

`npm test --prefix mcp` uses a synthetic HTTP server and the official SDK’s
in-memory client and stdio subprocess transport. It covers route/body mapping,
read/write authorization, isolation, strict selectors, tag pagination,
redirect refusal, cancellation, response bounds, CF Access headers, private
credential files, CLI setup and protocol-only stdout. No live account, token
or AI calls are part of the tests. Server authorization is independently
pinned by `crates/server/tests/library_tokens.rs` and [F21](../docs/web-port/API-TOKENS.md).

Protocol and host setup references:
[SDK server](https://ts.sdk.modelcontextprotocol.io/server),
[Claude Code](https://code.claude.com/docs/en/mcp),
[local MCP servers / Claude Desktop](https://modelcontextprotocol.io/docs/develop/connect-local-servers),
[Cloudflare Access service tokens](https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/).
