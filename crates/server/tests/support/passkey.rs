//! A software passkey: a browser (the WebAuthn client) and a platform
//! authenticator in one, for the tests.
//!
//! It does what the browser and the authenticator do, on the JSON forms
//! (`PublicKeyCredential.parse…OptionsFromJSON()` in, `toJSON()` out):
//!
//! - checks that the RP ID is the origin's host or a parent of it, on a
//!   secure origin (https, or localhost);
//! - creates discoverable credentials (ES256 keys, OpenSSL) holding the user
//!   handle, with `none` attestation, and refuses to create one when it
//!   holds a credential the options exclude;
//! - verifies the user (UV flag) unless told it cannot, and signs
//!   assertions over `authenticatorData || SHA-256(clientDataJSON)`;
//! - counts signatures, as security keys do, or, [`SoftPasskey::synced`],
//!   sends counter 0 with the backup flags, as iCloud Keychain and Google
//!   Password Manager do.
//!
//! [`SoftPasskey::clone_device`] copies keys and counters: a cloned
//! authenticator. The ceremony helpers drive the server's routes as the SPA
//! does. Written for these tests rather than taken from a crate:
//! passkey-rs, the maintained pure-Rust option, turns on serde_json's
//! `preserve_order` in every test build of the workspace, which would make
//! tests order JSON objects unlike production.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openssl::bn::{BigNum, BigNumContext};
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::sign::Signer;
use serde_json::{Value, json};
use url::Url;

use super::auth::{from_spa, session_cookie, spa};
use super::{TestState, json as body_json, send};

/// Authenticator data flags (WebAuthn §6.1).
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const BE: u8 = 0x08;
const BS: u8 = 0x10;
const AT: u8 = 0x40;

/// A credential the authenticator holds.
#[derive(Clone)]
struct Credential {
    id: Vec<u8>,
    rp_id: String,
    user_handle: Vec<u8>,
    key: PKey<Private>,
    counter: u32,
}

/// A browser on `origin` with a software passkey authenticator.
#[derive(Clone)]
pub struct SoftPasskey {
    origin: Url,
    credentials: Vec<Credential>,
    verifies: bool,
    synced: bool,
}

impl SoftPasskey {
    /// A browser on the public URL of `t`, with no passkey yet.
    pub fn for_app(t: &TestState) -> Self {
        Self::on(t.state.config().public_url.as_str())
    }

    /// A browser on `origin`, with no passkey yet.
    pub fn on(origin: &str) -> Self {
        Self {
            origin: Url::parse(origin).expect("an origin"),
            credentials: Vec::new(),
            verifies: true,
            synced: false,
        }
    }

    /// The same authenticator seen from another origin.
    pub fn at(&self, origin: &str) -> Self {
        Self {
            origin: Url::parse(origin).expect("an origin"),
            ..self.clone()
        }
    }

    /// An authenticator that cannot verify the user: it refuses ceremonies
    /// that require it, and signs others without the UV flag.
    pub fn without_user_verification(mut self) -> Self {
        self.verifies = false;
        self
    }

    /// A synced passkey provider: counter always 0, backup eligible and
    /// backed up.
    pub fn synced(mut self) -> Self {
        self.synced = true;
        self
    }

    /// A copy of the authenticator, keys and counters included: a clone.
    pub fn clone_device(&self) -> Self {
        self.clone()
    }

    /// How many credentials the authenticator holds.
    pub fn credentials(&self) -> usize {
        self.credentials.len()
    }

    /// `navigator.credentials.create({publicKey})`, then `toJSON()`.
    ///
    /// # Panics
    ///
    /// When the browser or the authenticator refuses.
    pub async fn create(&mut self, public_key: &Value) -> Value {
        self.try_create(public_key)
            .await
            .unwrap_or_else(|err| panic!("the authenticator refused to create: {err}"))
    }

    /// `navigator.credentials.create({publicKey})`, then `toJSON()`, or
    /// why the browser or the authenticator refused.
    pub async fn try_create(&mut self, options: &Value) -> Result<Value, String> {
        let rp_id = self.checked_rp_id(options["rp"]["id"].as_str())?;
        let user_handle = decode(&options["user"]["id"])?;
        let excluded = descriptor_ids(&options["excludeCredentials"])?;
        if self
            .credentials
            .iter()
            .any(|held| held.rp_id == rp_id && excluded.contains(&held.id))
        {
            return Err("InvalidStateError: a credential the options exclude".into());
        }
        let algorithms: Vec<i64> = options["pubKeyCredParams"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|param| param["alg"].as_i64())
            .collect();
        if !algorithms.contains(&-7) {
            return Err("NotSupportedError: ES256 is not offered".into());
        }
        let user_verified =
            self.user_verification(options["authenticatorSelection"]["userVerification"].as_str())?;

        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = EcKey::generate(&group).unwrap();
        let (mut x, mut y) = (BigNum::new().unwrap(), BigNum::new().unwrap());
        let mut context = BigNumContext::new().unwrap();
        key.public_key()
            .affine_coordinates(&group, &mut x, &mut y, &mut context)
            .unwrap();
        let key = PKey::from_ec_key(key).unwrap();
        let mut id = vec![0_u8; 16];
        openssl::rand::rand_bytes(&mut id).unwrap();

        let mut cose_key = Vec::new();
        cbor_head(5, 5, &mut cose_key);
        cbor_int(1, &mut cose_key);
        cbor_int(2, &mut cose_key); // kty: EC2
        cbor_int(3, &mut cose_key);
        cbor_int(-7, &mut cose_key); // alg: ES256
        cbor_int(-1, &mut cose_key);
        cbor_int(1, &mut cose_key); // crv: P-256
        cbor_int(-2, &mut cose_key);
        cbor_bytes(&x.to_vec_padded(32).unwrap(), &mut cose_key);
        cbor_int(-3, &mut cose_key);
        cbor_bytes(&y.to_vec_padded(32).unwrap(), &mut cose_key);

        let mut authenticator_data = self.authenticator_data(&rp_id, user_verified, 0, AT);
        authenticator_data.extend_from_slice(&[0; 16]); // AAGUID: none
        authenticator_data.extend_from_slice(&u16::try_from(id.len()).unwrap().to_be_bytes());
        authenticator_data.extend_from_slice(&id);
        authenticator_data.extend_from_slice(&cose_key);

        let mut attestation_object = Vec::new();
        cbor_head(5, 3, &mut attestation_object);
        cbor_text("fmt", &mut attestation_object);
        cbor_text("none", &mut attestation_object);
        cbor_text("attStmt", &mut attestation_object);
        cbor_head(5, 0, &mut attestation_object);
        cbor_text("authData", &mut attestation_object);
        cbor_bytes(&authenticator_data, &mut attestation_object);

        let client_data = self.client_data("webauthn.create", &options["challenge"])?;
        let public_key_der = key.public_key_to_der().unwrap();
        self.credentials.push(Credential {
            id: id.clone(),
            rp_id,
            user_handle,
            key,
            counter: 0,
        });
        Ok(json!({
            "id": encode(&id),
            "rawId": encode(&id),
            "type": "public-key",
            "response": {
                "clientDataJSON": encode(client_data.as_bytes()),
                "attestationObject": encode(&attestation_object),
                "authenticatorData": encode(&authenticator_data),
                "transports": ["internal", "hybrid"],
                "publicKey": encode(&public_key_der),
                "publicKeyAlgorithm": -7,
            },
            "authenticatorAttachment": "platform",
            "clientExtensionResults": { "credProps": { "rk": true } },
        }))
    }

    /// `navigator.credentials.get({publicKey})`, then `toJSON()`.
    ///
    /// # Panics
    ///
    /// When the browser or the authenticator refuses, or holds no
    /// credential the options allow.
    pub async fn get(&mut self, options: &Value) -> Value {
        self.try_get(options)
            .unwrap_or_else(|err| panic!("the authenticator refused to sign: {err}"))
    }

    fn try_get(&mut self, options: &Value) -> Result<Value, String> {
        let rp_id = self.checked_rp_id(options["rpId"].as_str())?;
        let allowed = descriptor_ids(&options["allowCredentials"])?;
        let user_verified = self.user_verification(options["userVerification"].as_str())?;
        let client_data = self.client_data("webauthn.get", &options["challenge"])?;
        let synced = self.synced;
        let index = self
            .credentials
            .iter()
            .position(|held| {
                held.rp_id == rp_id && (allowed.is_empty() || allowed.contains(&held.id))
            })
            .ok_or("NotAllowedError: no credential for this site")?;
        let counter = if synced {
            0
        } else {
            self.credentials[index].counter += 1;
            self.credentials[index].counter
        };
        let authenticator_data = self.authenticator_data(&rp_id, user_verified, counter, 0);
        let credential = &self.credentials[index];
        let mut signed = authenticator_data.clone();
        signed.extend_from_slice(&openssl::sha::sha256(client_data.as_bytes()));
        let mut signer = Signer::new(MessageDigest::sha256(), &credential.key).unwrap();
        signer.update(&signed).unwrap();
        let signature = signer.sign_to_vec().unwrap();
        Ok(json!({
            "id": encode(&credential.id),
            "rawId": encode(&credential.id),
            "type": "public-key",
            "response": {
                "clientDataJSON": encode(client_data.as_bytes()),
                "authenticatorData": encode(&authenticator_data),
                "signature": encode(&signature),
                "userHandle": encode(&credential.user_handle),
            },
            "authenticatorAttachment": "platform",
            "clientExtensionResults": {},
        }))
    }

    /// The RP ID the options name, if the origin may use it: its host, or a
    /// parent domain of it, on a secure origin.
    fn checked_rp_id(&self, rp_id: Option<&str>) -> Result<String, String> {
        let host = self.origin.host_str().ok_or("SecurityError: no host")?;
        let secure = self.origin.scheme() == "https" || host == "localhost";
        if !secure {
            return Err("SecurityError: not a secure context".into());
        }
        let rp_id = rp_id.unwrap_or(host);
        if host == rp_id || host.ends_with(&format!(".{rp_id}")) {
            Ok(rp_id.to_owned())
        } else {
            Err("SecurityError: the RP ID is not this origin's".into())
        }
    }

    /// Whether the user is verified, for the requirement `requirement`
    /// (absent: `preferred`).
    fn user_verification(&self, requirement: Option<&str>) -> Result<bool, String> {
        match requirement.unwrap_or("preferred") {
            "required" if !self.verifies => Err("NotAllowedError: cannot verify the user".into()),
            "discouraged" => Ok(false),
            _ => Ok(self.verifies),
        }
    }

    /// `clientDataJSON`, as browsers write it.
    fn client_data(&self, kind: &str, challenge: &Value) -> Result<String, String> {
        let challenge = encode(&decode(challenge)?);
        let origin = self.origin.origin().ascii_serialization();
        Ok(format!(
            r#"{{"type":"{kind}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        ))
    }

    fn authenticator_data(
        &self,
        rp_id: &str,
        user_verified: bool,
        counter: u32,
        extra: u8,
    ) -> Vec<u8> {
        let mut flags = UP | extra;
        if user_verified {
            flags |= UV;
        }
        if self.synced {
            flags |= BE | BS;
        }
        let mut data = openssl::sha::sha256(rp_id.as_bytes()).to_vec();
        data.push(flags);
        data.extend_from_slice(&counter.to_be_bytes());
        data
    }
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(value: &Value) -> Result<Vec<u8>, String> {
    let text = value.as_str().ok_or("TypeError: not a base64url string")?;
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|err| format!("EncodingError: {err}"))
}

/// The credential ids of a descriptor list (`excludeCredentials`,
/// `allowCredentials`); absent is empty.
fn descriptor_ids(list: &Value) -> Result<Vec<Vec<u8>>, String> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|descriptor| decode(&descriptor["id"]))
        .collect()
}

/// A CBOR head (RFC 8949 §3): major type and argument.
fn cbor_head(major: u8, argument: u64, out: &mut Vec<u8>) {
    let major = major << 5;
    match argument {
        0..=23 => out.push(major | u8::try_from(argument).unwrap()),
        24..=0xff => {
            out.push(major | 24);
            out.push(u8::try_from(argument).unwrap());
        }
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend_from_slice(&u16::try_from(argument).unwrap().to_be_bytes());
        }
        _ => {
            out.push(major | 26);
            out.extend_from_slice(&u32::try_from(argument).unwrap().to_be_bytes());
        }
    }
}

fn cbor_int(value: i64, out: &mut Vec<u8>) {
    if value >= 0 {
        cbor_head(0, value.unsigned_abs(), out);
    } else {
        cbor_head(1, (-1 - value).unsigned_abs(), out);
    }
}

fn cbor_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    cbor_head(2, bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn cbor_text(text: &str, out: &mut Vec<u8>) {
    cbor_head(3, text.len() as u64, out);
    out.extend_from_slice(text.as_bytes());
}

/// A `POST` of `body` as JSON.
pub fn post_body(uri: &str, body: &Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// `POST /me/passkeys/start` with the session `cookie`: the answer, which
/// must be 200.
pub async fn start_registration(app: &Router, t: &TestState, cookie: &str) -> Value {
    let request = spa(
        t,
        post_body("/api/v1/me/passkeys/start", &json!({})),
        cookie,
    );
    let response = send(app, request).await;
    assert_eq!(response.status(), StatusCode::OK, "registration start");
    body_json(response).await
}

/// Registers a passkey of `device` to the account of the session `cookie`,
/// as the Settings page does; returns the new passkey.
pub async fn register(
    app: &Router,
    t: &TestState,
    cookie: &str,
    device: &mut SoftPasskey,
    label: Option<&str>,
) -> Value {
    let start = start_registration(app, t, cookie).await;
    let credential = device.create(&start["publicKey"]).await;
    let mut body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    if let Some(label) = label {
        body["label"] = json!(label);
    }
    let response = send(app, spa(t, post_body("/api/v1/me/passkeys", &body), cookie)).await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "registration finish"
    );
    body_json(response).await
}

/// `POST /auth/passkeys/login/start`, as the sign-in page sends it: the
/// answer, which must be 200.
pub async fn start_sign_in(app: &Router, t: &TestState) -> Value {
    let request = from_spa(
        t,
        post_body("/api/v1/auth/passkeys/login/start", &json!({})),
    );
    let response = send(app, request).await;
    assert_eq!(response.status(), StatusCode::OK, "sign-in start");
    body_json(response).await
}

/// `POST /auth/passkeys/login/finish` with `ceremony_id` and `credential`.
pub fn finish_sign_in_request(
    t: &TestState,
    ceremony_id: &Value,
    credential: &Value,
) -> Request<Body> {
    let body = json!({ "ceremonyId": ceremony_id, "credential": credential });
    from_spa(t, post_body("/api/v1/auth/passkeys/login/finish", &body))
}

/// Signs in with `device`, username-less; returns the session cookie.
pub async fn sign_in_with(app: &Router, t: &TestState, device: &mut SoftPasskey) -> String {
    let start = start_sign_in(app, t).await;
    let credential = device.get(&start["publicKey"]).await;
    let response = send(
        app,
        finish_sign_in_request(t, &start["ceremonyId"], &credential),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "sign-in finish");
    session_cookie(&response).expect("the sign-in sets the session cookie")
}

/// Re-authenticates the session `cookie` with `device`.
pub async fn reauth_with(app: &Router, t: &TestState, cookie: &str, device: &mut SoftPasskey) {
    let request = spa(
        t,
        post_body("/api/v1/auth/reauth/start", &json!({ "method": "passkey" })),
        cookie,
    );
    let response = send(app, request).await;
    assert_eq!(response.status(), StatusCode::OK, "re-auth start");
    let start = body_json(response).await;
    let credential = device.get(&start["publicKey"]).await;
    let body = json!({
        "method": "passkey",
        "ceremonyId": start["ceremonyId"],
        "credential": credential,
    });
    let request = spa(t, post_body("/api/v1/auth/reauth/finish", &body), cookie);
    let response = send(app, request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "re-auth finish");
}
