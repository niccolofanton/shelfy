// Shared helpers: ULIDs (batch ids and Idempotency-Keys) and the panel's string lookup over
// src/i18n/messages/extension.ts.

import { describe, expect, it } from 'vitest';
import messages from '../../src/i18n/messages/extension';
import { createTranslate, hasMessage, pickLang, translate } from '../src/shared/i18n';
import { createUlid, isUlid, ulid, ulidTime } from '../src/shared/ulid';

describe('ULID', () => {
  it('encodes the time and sorts by creation, also within one millisecond', () => {
    let now = 1_760_000_000_000;
    const next = createUlid(
      () => now,
      (length) => new Uint8Array(length).fill(7),
    );
    const ids = [next(), next(), next()];
    now += 1;
    ids.push(next());
    for (const id of ids) expect(isUlid(id)).toBe(true);
    expect([...ids].sort()).toEqual(ids);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ulidTime(ids[0])).toBe(1_760_000_000_000);
    expect(ulidTime(ids[3])).toBe(1_760_000_000_001);
  });

  it('stays monotonic when the clock goes back', () => {
    let now = 2_000;
    const next = createUlid(
      () => now,
      (length) => new Uint8Array(length),
    );
    const a = next();
    now = 1_000;
    const b = next();
    expect(b > a).toBe(true);
    expect(ulidTime(b)).toBe(2_000);
  });

  it('the default generator uses crypto randomness', () => {
    const a = ulid();
    const b = ulid();
    expect(isUlid(a) && isUlid(b)).toBe(true);
    expect(a).not.toBe(b);
    expect(isUlid('01J0000000000000000000000I')).toBe(false); // I is not Crockford base32
  });
});

describe('extension strings', () => {
  it('have the same keys in English and Italian', () => {
    expect(Object.keys(messages.it).sort()).toEqual(Object.keys(messages.en).sort());
    for (const key of Object.keys(messages.en)) expect(hasMessage(key)).toBe(true);
  });

  it('pick Italian for an Italian browser, English otherwise', () => {
    expect(pickLang(['it-IT', 'en'])).toBe('it');
    expect(pickLang(['en-GB'])).toBe('en');
    expect(pickLang(['de-DE'])).toBe('en');
    expect(pickLang(undefined)).toBe('en');
  });

  it('interpolate variables and choose plurals by count', () => {
    expect(translate('en', 'queue.waiting', { count: 1 })).toBe('1 item waiting');
    expect(translate('en', 'queue.waiting', { count: 3 })).toBe('3 items waiting');
    expect(translate('it', 'queue.waiting', { count: 3 })).toBe('3 elementi in attesa');
    expect(translate('en', 'connection.server', { host: 'refs.niccolofanton.dev' })).toBe(
      'Server: refs.niccolofanton.dev',
    );
    expect(createTranslate('it')('missing.key')).toBe('missing.key');
    expect(translate('en', 'error.unknown')).toBe('Something went wrong ({code}).');
  });
});
