// Passkey ceremonies in the browser (WebAuthn, plan §2.11): the server's
// options in, the browser's answer out, both in the WebAuthn Level 3 JSON forms
// that the API speaks (crates/server/src/routes/passkeys/webauthn.rs).
//
// The JSON helpers of Level 3 do the conversion where the browser has them:
// `PublicKeyCredential.parseCreationOptionsFromJSON()`,
// `parseRequestOptionsFromJSON()` and `credential.toJSON()` (Chrome 129,
// Firefox 119, Safari and iOS 18.4). Older browsers, and the credential
// objects of some password-manager extensions, get the same conversion by
// hand: base64url strings to bytes and back.
//
// Safari runs a ceremony only inside a user gesture; it lets the gesture
// travel through `fetch` for 10 seconds, so a click may fetch the options
// first. Call these from the click handler.
import type { PasskeyFailureReason } from '@ui/api/account';
import type { components } from '../api/schema';

type Schemas = components['schemas'];
export type CreationOptionsJSON = Schemas['PublicKeyCredentialCreationOptionsJSON'];
export type RequestOptionsJSON = Schemas['PublicKeyCredentialRequestOptionsJSON'];
export type RegistrationJSON = Schemas['RegistrationResponseJSON'];
export type AuthenticationJSON = Schemas['AuthenticationResponseJSON'];

// A ceremony that failed in the browser (src/api/account.ts PasskeyFailure).
export class PasskeyError extends Error {
  readonly reason: PasskeyFailureReason;

  constructor(reason: PasskeyFailureReason, cause?: unknown) {
    super(`passkey ${reason}`);
    this.name = 'PasskeyError';
    this.reason = reason;
    if (cause !== undefined) (this as { cause?: unknown }).cause = cause;
  }
}

// The WebAuthn API of this window, or null: an old browser, or a page that is
// not a secure context (http on a host other than localhost).
function webAuthn(): {
  PKC: typeof PublicKeyCredential;
  credentials: CredentialsContainer;
} | null {
  if (typeof window === 'undefined' || typeof window.PublicKeyCredential !== 'function')
    return null;
  const credentials = typeof navigator === 'undefined' ? undefined : navigator.credentials;
  if (!credentials || typeof credentials.create !== 'function') return null;
  return { PKC: window.PublicKeyCredential, credentials };
}

// Whether this browser can use passkeys at all.
export function passkeysSupported(): boolean {
  return webAuthn() !== null;
}

// base64url (or standard base64, padded or not) to bytes.
export function fromBase64url(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, '+').replace(/_/g, '/').replace(/=+$/, '');
  const binary = atob(base64 + '='.repeat((4 - (base64.length % 4)) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

// Bytes to base64url without padding.
export function toBase64url(value: ArrayBuffer | ArrayBufferView): string {
  const bytes =
    value instanceof ArrayBuffer
      ? new Uint8Array(value)
      : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function descriptors(
  list: Schemas['PublicKeyCredentialDescriptorJSON'][] | undefined,
): PublicKeyCredentialDescriptor[] | undefined {
  return list?.map((d) => ({
    type: d.type as PublicKeyCredentialType,
    id: fromBase64url(d.id),
    ...(d.transports ? { transports: d.transports as AuthenticatorTransport[] } : {}),
  }));
}

// The options of `navigator.credentials.create()`.
export function creationOptions(json: CreationOptionsJSON): PublicKeyCredentialCreationOptions {
  const api = webAuthn();
  if (api && typeof api.PKC.parseCreationOptionsFromJSON === 'function') {
    try {
      return api.PKC.parseCreationOptionsFromJSON(
        json as unknown as PublicKeyCredentialCreationOptionsJSON,
      );
    } catch {
      /* a value this browser's parser refuses: convert by hand */
    }
  }
  return {
    ...(json as unknown as PublicKeyCredentialCreationOptions),
    challenge: fromBase64url(json.challenge),
    user: { ...json.user, id: fromBase64url(json.user.id) },
    pubKeyCredParams: json.pubKeyCredParams.map((p) => ({
      type: p.type as PublicKeyCredentialType,
      alg: p.alg,
    })),
    excludeCredentials: descriptors(json.excludeCredentials),
  };
}

// The options of `navigator.credentials.get()`.
export function requestOptions(json: RequestOptionsJSON): PublicKeyCredentialRequestOptions {
  const api = webAuthn();
  if (api && typeof api.PKC.parseRequestOptionsFromJSON === 'function') {
    try {
      return api.PKC.parseRequestOptionsFromJSON(
        json as unknown as PublicKeyCredentialRequestOptionsJSON,
      );
    } catch {
      /* convert by hand */
    }
  }
  return {
    ...(json as unknown as PublicKeyCredentialRequestOptions),
    challenge: fromBase64url(json.challenge),
    allowCredentials: descriptors(json.allowCredentials),
    userVerification: json.userVerification as UserVerificationRequirement,
  };
}

function bytesOrUndefined(read: (() => ArrayBuffer | null) | undefined): string | undefined {
  try {
    const value = read?.();
    return value ? toBase64url(value) : undefined;
  } catch {
    return undefined;
  }
}

// What `credential.toJSON()` returns, from the credential's fields.
function toJSONByHand(credential: PublicKeyCredential): RegistrationJSON | AuthenticationJSON {
  const common = {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    ...(credential.authenticatorAttachment
      ? { authenticatorAttachment: credential.authenticatorAttachment }
      : {}),
    clientExtensionResults: (credential.getClientExtensionResults?.() ?? {}) as Record<
      string,
      never
    >,
  };
  const response = credential.response;
  if ('attestationObject' in response) {
    const attestation = response as AuthenticatorAttestationResponse;
    const algorithm = attestation.getPublicKeyAlgorithm?.();
    const transports = attestation.getTransports?.();
    return {
      ...common,
      response: {
        clientDataJSON: toBase64url(attestation.clientDataJSON),
        attestationObject: toBase64url(attestation.attestationObject),
        authenticatorData: bytesOrUndefined(attestation.getAuthenticatorData?.bind(attestation)),
        publicKey: bytesOrUndefined(attestation.getPublicKey?.bind(attestation)),
        ...(typeof algorithm === 'number' ? { publicKeyAlgorithm: algorithm } : {}),
        ...(Array.isArray(transports) ? { transports } : {}),
      },
    };
  }
  const assertion = response as AuthenticatorAssertionResponse;
  return {
    ...common,
    response: {
      clientDataJSON: toBase64url(assertion.clientDataJSON),
      authenticatorData: toBase64url(assertion.authenticatorData),
      signature: toBase64url(assertion.signature),
      ...(assertion.userHandle ? { userHandle: toBase64url(assertion.userHandle) } : {}),
    },
  };
}

// The credential in the JSON form the API takes.
export function credentialJSON(credential: PublicKeyCredential): unknown {
  if (typeof credential.toJSON === 'function') {
    try {
      return credential.toJSON();
    } catch {
      /* a credential object that cannot serialize itself */
    }
  }
  return toJSONByHand(credential);
}

// The reason of a ceremony that failed, from the browser's DOMException.
export function failureReason(err: unknown): PasskeyFailureReason {
  const name = err instanceof Error || err instanceof DOMException ? err.name : '';
  switch (name) {
    case 'NotAllowedError':
    case 'AbortError':
      return 'cancelled';
    case 'InvalidStateError':
      return 'exists';
    case 'NotSupportedError':
    case 'ConstraintError':
      return 'unsupported';
    case 'SecurityError':
      return 'origin';
    default:
      return 'failed';
  }
}

async function ceremony<T>(run: () => Promise<Credential | null>): Promise<T> {
  let credential: Credential | null;
  try {
    credential = await run();
  } catch (err) {
    throw new PasskeyError(failureReason(err), err);
  }
  if (!credential || credential.type !== 'public-key') throw new PasskeyError('cancelled');
  return credentialJSON(credential as PublicKeyCredential) as T;
}

// Creates a passkey with this device's authenticator: the answer for
// `POST /me/passkeys`.
export function createPasskey(options: CreationOptionsJSON): Promise<RegistrationJSON> {
  const api = webAuthn();
  if (!api) return Promise.reject(new PasskeyError('unsupported'));
  return ceremony<RegistrationJSON>(() =>
    api.credentials.create({ publicKey: creationOptions(options) }),
  );
}

// Signs the server's challenge with a passkey: the answer for a sign-in or a
// re-authentication.
export function signWithPasskey(options: RequestOptionsJSON): Promise<AuthenticationJSON> {
  const api = webAuthn();
  if (!api) return Promise.reject(new PasskeyError('unsupported'));
  return ceremony<AuthenticationJSON>(() =>
    api.credentials.get({ publicKey: requestOptions(options) }),
  );
}
