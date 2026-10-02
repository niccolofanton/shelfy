#!/usr/bin/env node
// scripts/live/session.mjs — P1-26 prep: sessions and tokens for the live
// checks, run by the lead against the live host (never the owner's own
// browser session, per P1 lane rule 8 "no owner session in automation").
//
// The lead mints a login link out of band with the admin CLI:
//
//   shelfy-server admin login-link --email "$SHELFY_OWNER_EMAIL" \
//     --public-url https://refs.niccolofanton.dev
//
// This tool redeems that link, mints a short-lived read API token while the
// sign-in is still "recent" (RecentAuth, a 5-minute window:
// crates/server/src/auth/session.rs), and revokes both when the run is
// done. Every request against the live host carries the Cloudflare Access
// service-token headers from `--headers` (a 0600 file, `Name: value` per
// line — see scripts/live/lib.mjs#readHeadersFile), so the secret never
// reaches argv or a log. Nothing here ever prints a cookie or token value;
// the session and token files it writes are 0600 and meant to live only as
// long as the run.
//
// Usage:
//   node session.mjs redeem <login-link-url> --out session.json [--headers access.headers]
//   node session.mjs token --session session.json --out token.json
//     [--label "p1-26 live-check"] [--scopes lookup] [--headers access.headers]
//   node session.mjs revoke --session session.json [--token token.json] [--headers access.headers]
//
// `redeem` reads the base URL and the token straight from the login-link
// URL (its origin and its `#` fragment), so there is no separate `--base`
// flag; `token` and `revoke` read it back from the session file.

import { parseArgs } from 'node:util';

import {
  apiCall,
  ProblemError,
  readHeadersFile,
  readJsonFile,
  removeIfExists,
  usageError,
  writeSecretJson,
} from './lib.mjs';

/** Reads a file that may not exist yet, returning `null` instead of
 * throwing — `revoke` is idempotent, so a missing session or token file is
 * not an error, only a sign that a previous run already cleaned up. */
function readIfExists(path) {
  if (!path) return null;
  try {
    return readJsonFile(path);
  } catch (err) {
    if (err.code === 'ENOENT') return null;
    throw err;
  }
}

async function cmdRedeem(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    options: {
      out: { type: 'string' },
      headers: { type: 'string' },
    },
    allowPositionals: true,
  });
  const linkUrl = positionals[0];
  if (!linkUrl || !values.out) {
    usageError('session.mjs redeem <login-link-url> --out <file> [--headers <file>]');
  }
  let url;
  try {
    url = new URL(linkUrl);
  } catch {
    throw new Error(`not a URL: ${linkUrl}`);
  }
  const base = url.origin;
  const token = url.hash.replace(/^#/, '');
  if (!token) {
    throw new Error(
      'the login-link URL has no token in its #fragment; pass the full URL printed by `admin login-link`',
    );
  }
  const accessHeaders = readHeadersFile(values.headers);

  let redeemed;
  try {
    redeemed = await apiCall(base, '/api/v1/auth/magic-links/redeem', {
      method: 'POST',
      body: { token },
      origin: true,
      accessHeaders,
    });
  } catch (err) {
    if (err instanceof ProblemError && err.code === 'invalid_link') {
      throw new Error(
        'the link is unknown, already used, or expired (links work once, 15 minutes): mint a fresh one with `admin login-link` and retry',
      );
    }
    throw err;
  }
  const setCookies =
    typeof redeemed.headers.getSetCookie === 'function'
      ? redeemed.headers.getSetCookie()
      : [redeemed.headers.get('set-cookie')].filter(Boolean);
  const raw = setCookies.find((c) => c.startsWith('__Host-shelfy_session='));
  if (!raw) {
    throw new Error('redeemed the link, but the response set no session cookie (unexpected)');
  }
  const cookie = raw.split(';')[0].trim();
  const redeemedAt = new Date().toISOString();

  // Confirms the sign-in and captures the user id, for the record — never
  // the email, which this tool never needs again.
  const me = await apiCall(base, '/api/v1/me', { cookie, accessHeaders });

  writeSecretJson(values.out, {
    base,
    cookie,
    userId: me.json.id,
    redeemedAt,
  });
  console.error(`signed in as ${me.json.id} on ${base}`);
  console.error(`session written to ${values.out} (0600); mint a token within 5 minutes of now`);
}

async function cmdToken(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      session: { type: 'string' },
      out: { type: 'string' },
      headers: { type: 'string' },
      label: { type: 'string', default: 'p1-26 live-check' },
      scopes: { type: 'string', default: 'lookup' },
    },
  });
  if (!values.session || !values.out) {
    usageError(
      'session.mjs token --session <file> --out <file> [--label <text>] [--scopes lookup] [--headers <file>]',
    );
  }
  const session = readJsonFile(values.session);
  const accessHeaders = readHeadersFile(values.headers);
  const scopes = values.scopes
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);

  let created;
  try {
    created = await apiCall(session.base, '/api/v1/me/tokens', {
      method: 'POST',
      body: { kind: 'extension', label: values.label, scopes },
      cookie: session.cookie,
      origin: true,
      accessHeaders,
    });
  } catch (err) {
    if (err instanceof ProblemError && err.code === 'reauth_required') {
      throw new Error(
        'minting a token needs a sign-in from the last 5 minutes, and this session is older: redeem a fresh login link and run `token` again right away',
      );
    }
    throw err;
  }

  writeSecretJson(values.out, {
    base: session.base,
    id: created.json.apiToken.id,
    token: created.json.token,
    scopes: created.json.apiToken.scopes,
    createdAt: new Date().toISOString(),
  });
  console.error(
    `minted token ${created.json.apiToken.id} (scopes: ${created.json.apiToken.scopes.join(' ')})`,
  );
  console.error(`token written to ${values.out} (0600)`);
}

async function cmdRevoke(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      session: { type: 'string' },
      token: { type: 'string' },
      headers: { type: 'string' },
    },
  });
  if (!values.session) {
    usageError('session.mjs revoke --session <file> [--token <file>] [--headers <file>]');
  }
  const accessHeaders = readHeadersFile(values.headers);
  const session = readIfExists(values.session);

  if (values.token) {
    const tokenFile = readIfExists(values.token);
    if (tokenFile && session) {
      try {
        await apiCall(session.base, `/api/v1/me/tokens/${tokenFile.id}`, {
          method: 'DELETE',
          cookie: session.cookie,
          origin: true,
          accessHeaders,
        });
        console.error(`revoked token ${tokenFile.id}`);
      } catch (err) {
        // 404: another run already revoked it. 401: the session is already
        // gone, so there is no credential left to revoke it with either —
        // both are "nothing more to do here", not a failure.
        if (err instanceof ProblemError && (err.status === 404 || err.status === 401)) {
          console.error(
            `token ${tokenFile.id}: ${err.status === 401 ? 'session already gone' : 'already revoked'}`,
          );
        } else {
          throw err;
        }
      }
    } else if (tokenFile && !session) {
      console.error(
        `no session file; cannot revoke token ${tokenFile.id} (it may still be active — check \`GET /me/tokens\`)`,
      );
    }
    removeIfExists(values.token);
  }

  if (session) {
    await apiCall(session.base, '/api/v1/auth/logout', {
      method: 'POST',
      cookie: session.cookie,
      origin: true,
      accessHeaders,
    });
    console.error('session signed out');
  }
  removeIfExists(values.session);
  console.error('done: session and token files removed');
}

async function main() {
  const sub = process.argv[2];
  const rest = process.argv.slice(3);
  if (sub === 'redeem') return cmdRedeem(rest);
  if (sub === 'token') return cmdToken(rest);
  if (sub === 'revoke') return cmdRevoke(rest);
  usageError(
    "session.mjs <redeem|token|revoke> ...  (see the top of this file for each command's options)",
  );
  return undefined;
}

main().catch((err) => {
  console.error(`error: ${err.message}`);
  process.exitCode = 1;
});
