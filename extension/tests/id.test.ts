// The committed manifest key pins the extension's ID (P2-G6): Chrome's ID is the first 128 bits
// of the SHA-256 of the DER SubjectPublicKeyInfo, written with the letters a–p.

import { createHash, createPublicKey } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { EXTENSION_ID, EXTENSION_PUBLIC_KEY } from '../src/id';

function chromeExtensionId(base64Key: string): string {
  const hex = createHash('sha256').update(Buffer.from(base64Key, 'base64')).digest('hex');
  return [...hex.slice(0, 32)]
    .map((digit) => String.fromCharCode(97 + parseInt(digit, 16)))
    .join('');
}

describe('extension id', () => {
  it('is the id Chrome derives from the committed public key', () => {
    expect(EXTENSION_ID).toMatch(/^[a-p]{32}$/);
    expect(chromeExtensionId(EXTENSION_PUBLIC_KEY)).toBe(EXTENSION_ID);
  });

  it('the key is an RSA public key (SPKI DER), and no private key is committed', () => {
    const key = createPublicKey({
      key: Buffer.from(EXTENSION_PUBLIC_KEY, 'base64'),
      format: 'der',
      type: 'spki',
    });
    expect(key.asymmetricKeyType).toBe('rsa');
    expect(key.asymmetricKeyDetails?.modulusLength).toBe(2048);
    const source = readFileSync(new URL('../src/id.ts', import.meta.url), 'utf8');
    expect(source).not.toMatch(/PRIVATE KEY/);
  });

  it('src/id.ts has no imports, so the web app can import EXTENSION_ID alone', () => {
    const source = readFileSync(new URL('../src/id.ts', import.meta.url), 'utf8');
    expect(source).not.toMatch(/^\s*import\s/m);
  });
});
