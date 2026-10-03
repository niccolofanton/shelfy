// The extension's fixed ID (P2-G6). Chrome derives an unpacked extension's ID from its folder
// path, unless the manifest carries a `key`: then the ID is the first 128 bits of the SHA-256
// of that public key (DER SubjectPublicKeyInfo), written with the letters a–p. Committing the
// public key below gives every unpacked build the same ID, so the SPA can address it
// (`chrome.runtime.sendMessage(EXTENSION_ID, …)`, C9) and `externally_connectable` works.
//
// Only the public half exists: no private key was kept, because builds are loaded unpacked (E3)
// and never packed as a CRX. extension/tests/id.test.ts recomputes the ID from the key.
//
// Plain constants with no imports: the web app (P2-12) imports EXTENSION_ID from this file.

/** Manifest `key`: base64 of the DER SubjectPublicKeyInfo of an RSA-2048 public key. */
export const EXTENSION_PUBLIC_KEY =
  'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAywT4l3fvOEesLKJ+fRZq8jg0evMpDv62kUtF9pkLQAX3w5bLYQZEZl6vKoePpbXtg7lWKJJZzjcnuoMU8QJ9x+7fFQ0iIOjJROR6lIPWXYb+YpvVIyO02nkqFwnwHzSnCSWt/TDBDOtSTBM1cMU1Ex+NvyaukfMeDeZ0RVr7xWURdA1LsK2JsNA75xzlxjNxd3/m9Hygilyuj0hYjsb+TxM4ZDcraEz42j5TQchV3yoZFNMqlpQDCBXWzn5g5yUX0uduEvE4zUxFOcyqyhBrRvCF1tbY+JB1INilSzdlGoGuBgDOLHMAljs0za2aqK4xtDBVFG1Abh5YVF1bmsYt+QIDAQAB';

/** The ID of every build of this extension, derived from EXTENSION_PUBLIC_KEY. */
export const EXTENSION_ID = 'ckdhhkeliaagkhdgogjacajofoidkbem';
