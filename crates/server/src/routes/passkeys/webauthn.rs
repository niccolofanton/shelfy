//! The WebAuthn JSON forms (WebAuthn Level 3 §5.1.8, §5.4, §5.5) as OpenAPI
//! schemas, under their WebAuthn names.
//!
//! Binary values are base64url strings without padding. The options are the
//! input of `PublicKeyCredential.parseCreationOptionsFromJSON()` and
//! `parseRequestOptionsFromJSON()`; the answers are what the credential's
//! `toJSON()` returns (the server also accepts `extensions` for
//! `clientExtensionResults`, and padded or standard base64). These types only
//! document the JSON: the server reads and writes it with webauthn-rs's own
//! types, and `tests/passkeys.rs` checks real ceremonies against these
//! schemas.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `PublicKeyCredentialCreationOptionsJSON`: what
/// `navigator.credentials.create()` needs to create a passkey.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PublicKeyCredentialCreationOptionsJSON)]
pub struct CreationOptions {
    /// The relying party: this server.
    pub rp: RelyingParty,
    /// The account the passkey is for.
    pub user: UserEntity,
    /// 32 random bytes, base64url.
    pub challenge: String,
    /// The key types accepted, preferred first: ES256 (-7), RS256 (-257).
    pub pub_key_cred_params: Vec<CredentialParameters>,
    /// How long the ceremony may take, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub timeout: Option<u32>,
    /// The account's passkeys: an authenticator holding one of them does not
    /// create another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub exclude_credentials: Option<Vec<CredentialDescriptor>>,
    /// What kind of credential to create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub authenticator_selection: Option<AuthenticatorSelection>,
    /// `none`: no attestation is asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub attestation: Option<String>,
    /// Hints for the browser's UI (`client-device`, `security-key`,
    /// `hybrid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub hints: Option<Vec<String>>,
    /// Attestation formats accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub attestation_formats: Option<Vec<String>>,
    /// Extension inputs (`credProps`, `credentialProtectionPolicy`, `uvm`);
    /// a browser ignores those it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>, nullable = false)]
    pub extensions: Option<serde_json::Value>,
}

/// `PublicKeyCredentialRequestOptionsJSON`: what
/// `navigator.credentials.get()` needs to sign with a passkey.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PublicKeyCredentialRequestOptionsJSON)]
pub struct RequestOptions {
    /// 32 random bytes, base64url.
    pub challenge: String,
    /// How long the ceremony may take, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub timeout: Option<u32>,
    /// The relying party ID: the public host name.
    pub rp_id: String,
    /// Empty to sign in: the browser offers the passkeys it holds for the RP
    /// ID. The account's passkeys to re-authenticate.
    pub allow_credentials: Vec<CredentialDescriptor>,
    /// `required`: the authenticator verifies the user (biometrics, PIN).
    pub user_verification: String,
    /// Hints for the browser's UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub hints: Option<Vec<String>>,
    /// Extension inputs (`uvm`); a browser ignores those it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>, nullable = false)]
    pub extensions: Option<serde_json::Value>,
}

/// `PublicKeyCredentialRpEntity`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[schema(as = PublicKeyCredentialRpEntity)]
pub struct RelyingParty {
    /// The RP ID: the public host name.
    pub id: String,
    /// `Shelfy`.
    pub name: String,
}

/// `PublicKeyCredentialUserEntityJSON`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PublicKeyCredentialUserEntityJSON)]
pub struct UserEntity {
    /// The user handle: 16 opaque bytes, base64url, stable per account.
    pub id: String,
    /// The account's email, shown in the authenticator's account picker.
    pub name: String,
    /// The account's email too.
    pub display_name: String,
}

/// `PublicKeyCredentialParameters`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[schema(as = PublicKeyCredentialParameters)]
pub struct CredentialParameters {
    /// `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// A COSE algorithm: -7 (ES256) or -257 (RS256).
    pub alg: i64,
}

/// `PublicKeyCredentialDescriptorJSON`: names a passkey.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[schema(as = PublicKeyCredentialDescriptorJSON)]
pub struct CredentialDescriptor {
    /// `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The credential id, base64url.
    pub id: String,
    /// How the authenticator is reached (`internal`, `hybrid`, `usb`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub transports: Option<Vec<String>>,
}

/// `AuthenticatorSelectionCriteria`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AuthenticatorSelectionCriteria)]
pub struct AuthenticatorSelection {
    /// `platform` or `cross-platform`; absent: either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub authenticator_attachment: Option<String>,
    /// `required`: a discoverable credential, for username-less sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub resident_key: Option<String>,
    /// `true`, the WebAuthn Level 1 form of `residentKey: required`.
    pub require_resident_key: bool,
    /// `required`: the authenticator verifies the user.
    pub user_verification: String,
}

/// `RegistrationResponseJSON`: what the new credential's `toJSON()` returns.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RegistrationResponseJSON)]
pub struct RegistrationResponse {
    /// The credential id, base64url.
    pub id: String,
    /// The credential id, base64url.
    pub raw_id: String,
    /// `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The authenticator's answer.
    pub response: AttestationResponse,
    /// `platform` or `cross-platform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub authenticator_attachment: Option<String>,
    /// Extension outputs (`credProps`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>, nullable = false)]
    pub client_extension_results: Option<serde_json::Value>,
}

/// `AuthenticatorAttestationResponseJSON`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AuthenticatorAttestationResponseJSON)]
pub struct AttestationResponse {
    /// The client data, base64url.
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// The attestation object (CBOR), base64url.
    pub attestation_object: String,
    /// The authenticator data, base64url.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub authenticator_data: Option<String>,
    /// How the authenticator is reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub transports: Option<Vec<String>>,
    /// The public key (DER `SubjectPublicKeyInfo`), base64url.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub public_key: Option<String>,
    /// Its COSE algorithm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub public_key_algorithm: Option<i64>,
}

/// `AuthenticationResponseJSON`: what the credential's `toJSON()` returns
/// after `navigator.credentials.get()`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AuthenticationResponseJSON)]
pub struct AuthenticationResponse {
    /// The credential id, base64url.
    pub id: String,
    /// The credential id, base64url.
    pub raw_id: String,
    /// `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The authenticator's answer.
    pub response: AssertionResponse,
    /// `platform` or `cross-platform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub authenticator_attachment: Option<String>,
    /// Extension outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>, nullable = false)]
    pub client_extension_results: Option<serde_json::Value>,
}

/// `AuthenticatorAssertionResponseJSON`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AuthenticatorAssertionResponseJSON)]
pub struct AssertionResponse {
    /// The client data, base64url.
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// The authenticator data, base64url.
    pub authenticator_data: String,
    /// The signature, base64url.
    pub signature: String,
    /// The user handle, base64url: required to sign in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub user_handle: Option<String>,
}
