// Every problem code of the API maps to a message in every language the app
// ships (plan §2.9: the server sends codes, the client writes the prose). The
// codes come from the OpenAPI document, so a code the server adds fails here
// until src/i18n/messages/errors.ts has its `code.<code>` line.
import { describe, it, expect } from 'vitest';
import { LANGUAGES, messages, translate } from '@ui/i18n';
import { errorCodeOf, errorMessageKey } from '@ui/api/errors';
import errors from '@ui/i18n/messages/errors';
import openapi from '../../crates/server/openapi.json';
import { ApiError, type ErrorCode } from '../src/api/http';

const API_CODES: string[] = openapi.components.schemas.ErrorCode.enum;

// The generated type and the document agree, and `network` is the client's own.
const CLIENT_CODES: (ErrorCode | 'network')[] = [...(API_CODES as ErrorCode[]), 'network'];

describe('problem codes', () => {
  it('reads the codes of the API document', () => {
    expect(API_CODES).toContain('not_found');
    expect(API_CODES.length).toBeGreaterThan(15);
  });

  it('has a message for every code in every language', () => {
    for (const { code: lang } of LANGUAGES) {
      for (const code of CLIENT_CODES) {
        const value = messages[lang][`errors.code.${code}`];
        expect(typeof value === 'string' && value.length > 0, `${lang}: ${code}`).toBe(true);
      }
    }
  });

  it('has no message for a code the API does not define', () => {
    const known = new Set<string>(CLIENT_CODES);
    for (const key of Object.keys(errors.en)) {
      if (key.startsWith('code.')) expect(known.has(key.slice(5)), key).toBe(true);
    }
  });

  it('keeps the same keys in every language', () => {
    expect(Object.keys(errors.it).sort()).toEqual(Object.keys(errors.en).sort());
  });
});

describe('errorMessageKey', () => {
  it('names the message of a coded failure', () => {
    const err = new ApiError(503, 'unavailable', 'database busy');
    expect(errorCodeOf(err)).toBe('unavailable');
    expect(errorMessageKey(err)).toBe('code.unavailable');
    expect(translate('en', `errors.${errorMessageKey(err)}`)).toBe(
      'The server is busy. Try again shortly.',
    );
    expect(errorMessageKey(new ApiError(0, 'network'))).toBe('code.network');
  });

  it('has none for a failure without a known code', () => {
    expect(errorMessageKey(new Error('IPC failed'))).toBeNull();
    expect(errorMessageKey({ code: 'SQLITE_BUSY' })).toBeNull();
    expect(errorMessageKey({ code: 20 })).toBeNull();
    expect(errorMessageKey(null)).toBeNull();
    expect(errorMessageKey('not_found')).toBeNull();
  });
});
