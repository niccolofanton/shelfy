// Passkey ceremonies in the browser (web/src/auth/passkeys.ts): the WebAuthn
// Level 3 JSON in and out, by the browser's own helpers or by hand, and the
// browser's failures as reasons.
import { describe, it, expect, vi, afterEach } from 'vitest';
import {
  PasskeyError,
  createPasskey,
  creationOptions,
  credentialJSON,
  failureReason,
  fromBase64url,
  passkeysSupported,
  requestOptions,
  signWithPasskey,
  toBase64url,
  type CreationOptionsJSON,
  type RequestOptionsJSON,
} from '../src/auth/passkeys';
import { isPasskeyFailure } from '@ui/api/account';

const bytes = (...values: number[]): ArrayBuffer => new Uint8Array(values).buffer;

const CREATION: CreationOptionsJSON = {
  rp: { id: 'localhost', name: 'Shelfy' },
  user: { id: 'AQID', name: 'o@x.test', displayName: 'o@x.test' },
  challenge: '_-8',
  pubKeyCredParams: [
    { type: 'public-key', alg: -7 },
    { type: 'public-key', alg: -257 },
  ],
  timeout: 300000,
  excludeCredentials: [{ type: 'public-key', id: 'BAUG', transports: ['internal'] }],
  authenticatorSelection: {
    residentKey: 'required',
    requireResidentKey: true,
    userVerification: 'required',
  },
  attestation: 'none',
};

const REQUEST: RequestOptionsJSON = {
  challenge: 'AAEC',
  timeout: 300000,
  rpId: 'localhost',
  allowCredentials: [],
  userVerification: 'required',
};

type Win = { PublicKeyCredential?: unknown };
const win = window as unknown as Win;

function installWebAuthn(pkc: object, credentials: object): void {
  win.PublicKeyCredential = Object.assign(function PublicKeyCredential() {}, pkc);
  Object.defineProperty(navigator, 'credentials', { configurable: true, value: credentials });
}

afterEach(() => {
  delete win.PublicKeyCredential;
  delete (navigator as unknown as { credentials?: unknown }).credentials;
});

describe('base64url', () => {
  it('round-trips bytes without padding, and reads standard base64 too', () => {
    expect(toBase64url(bytes(0xfb, 0xff))).toBe('-_8');
    expect(toBase64url(new Uint8Array([1, 2, 3]))).toBe('AQID');
    expect(new Uint8Array(fromBase64url('-_8'))).toEqual(new Uint8Array([0xfb, 0xff]));
    expect(new Uint8Array(fromBase64url('+/8='))).toEqual(new Uint8Array([0xfb, 0xff]));
    expect(new Uint8Array(fromBase64url(''))).toEqual(new Uint8Array([]));
  });
});

describe('options', () => {
  it('uses the browser’s JSON parsers when it has them', () => {
    const parsed = { parsed: true };
    const parseCreationOptionsFromJSON = vi.fn(() => parsed);
    const parseRequestOptionsFromJSON = vi.fn(() => parsed);
    installWebAuthn(
      { parseCreationOptionsFromJSON, parseRequestOptionsFromJSON },
      { create: vi.fn(), get: vi.fn() },
    );
    expect(creationOptions(CREATION)).toBe(parsed);
    expect(requestOptions(REQUEST)).toBe(parsed);
    expect(parseCreationOptionsFromJSON).toHaveBeenCalledWith(CREATION);
  });

  it('converts by hand without them, or when they refuse a value', () => {
    installWebAuthn(
      {
        parseCreationOptionsFromJSON: () => {
          throw new TypeError('unknown member');
        },
      },
      { create: vi.fn(), get: vi.fn() },
    );
    const create = creationOptions(CREATION);
    expect(new Uint8Array(create.challenge as ArrayBuffer)).toEqual(new Uint8Array([0xff, 0xef]));
    expect(new Uint8Array(create.user.id as ArrayBuffer)).toEqual(new Uint8Array([1, 2, 3]));
    expect(create.excludeCredentials?.[0]).toMatchObject({
      type: 'public-key',
      transports: ['internal'],
    });
    expect(new Uint8Array(create.excludeCredentials?.[0].id as ArrayBuffer)).toEqual(
      new Uint8Array([4, 5, 6]),
    );
    expect(create.authenticatorSelection?.residentKey).toBe('required');

    const get = requestOptions(REQUEST);
    expect(new Uint8Array(get.challenge as ArrayBuffer)).toEqual(new Uint8Array([0, 1, 2]));
    expect(get.allowCredentials).toEqual([]);
    expect(get.rpId).toBe('localhost');
    expect(get.userVerification).toBe('required');
  });
});

describe('credentialJSON', () => {
  it('prefers the credential’s own toJSON', () => {
    const json = { id: 'x' };
    const credential = { toJSON: () => json } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toBe(json);
  });

  it('serializes a new passkey by hand', () => {
    const credential = {
      id: 'AQID',
      rawId: bytes(1, 2, 3),
      type: 'public-key',
      authenticatorAttachment: 'platform',
      getClientExtensionResults: () => ({ credProps: { rk: true } }),
      response: {
        clientDataJSON: bytes(7),
        attestationObject: bytes(8),
        getAuthenticatorData: () => bytes(9),
        getPublicKey: () => null,
        getPublicKeyAlgorithm: () => -7,
        getTransports: () => ['internal', 'hybrid'],
      },
    } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toEqual({
      id: 'AQID',
      rawId: 'AQID',
      type: 'public-key',
      authenticatorAttachment: 'platform',
      clientExtensionResults: { credProps: { rk: true } },
      response: {
        clientDataJSON: 'Bw',
        attestationObject: 'CA',
        authenticatorData: 'CQ',
        publicKey: undefined,
        publicKeyAlgorithm: -7,
        transports: ['internal', 'hybrid'],
      },
    });
  });

  it('serializes a signature by hand, with its user handle', () => {
    const credential = {
      id: 'AQID',
      rawId: bytes(1, 2, 3),
      type: 'public-key',
      authenticatorAttachment: null,
      getClientExtensionResults: () => ({}),
      response: {
        clientDataJSON: bytes(1),
        authenticatorData: bytes(2),
        signature: bytes(3),
        userHandle: bytes(4),
      },
    } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toEqual({
      id: 'AQID',
      rawId: 'AQID',
      type: 'public-key',
      clientExtensionResults: {},
      response: {
        clientDataJSON: 'AQ',
        authenticatorData: 'Ag',
        signature: 'Aw',
        userHandle: 'BA',
      },
    });
  });
});

describe('ceremonies', () => {
  it('says passkeys are missing without WebAuthn', async () => {
    expect(passkeysSupported()).toBe(false);
    const err = await signWithPasskey(REQUEST).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(PasskeyError);
    expect((err as PasskeyError).reason).toBe('unsupported');
    expect(isPasskeyFailure(err)).toBe(true);
  });

  it('runs the browser’s ceremony and answers its JSON', async () => {
    const answer = { id: 'cred' };
    const get = vi.fn().mockResolvedValue({ type: 'public-key', toJSON: () => answer });
    installWebAuthn({}, { create: vi.fn(), get });
    expect(passkeysSupported()).toBe(true);
    await expect(signWithPasskey(REQUEST)).resolves.toBe(answer);
    expect(get.mock.calls[0][0].publicKey.rpId).toBe('localhost');
  });

  it('turns the browser’s failures into reasons', async () => {
    const create = vi
      .fn()
      .mockRejectedValueOnce(new DOMException('cancelled', 'NotAllowedError'))
      .mockRejectedValueOnce(new DOMException('excluded', 'InvalidStateError'))
      .mockResolvedValueOnce(null);
    installWebAuthn({}, { create, get: vi.fn() });
    const reasons: string[] = [];
    for (let i = 0; i < 3; i++) {
      reasons.push(
        await createPasskey(CREATION).then(
          () => 'ok',
          (err: PasskeyError) => err.reason,
        ),
      );
    }
    expect(reasons).toEqual(['cancelled', 'exists', 'cancelled']);
  });

  it('maps each DOMException name', () => {
    const reason = (name: string) => failureReason(new DOMException('x', name));
    expect(reason('NotAllowedError')).toBe('cancelled');
    expect(reason('AbortError')).toBe('cancelled');
    expect(reason('InvalidStateError')).toBe('exists');
    expect(reason('NotSupportedError')).toBe('unsupported');
    expect(reason('SecurityError')).toBe('origin');
    expect(reason('UnknownError')).toBe('failed');
    expect(failureReason('nope')).toBe('failed');
  });
});
