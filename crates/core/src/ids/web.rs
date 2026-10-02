//! Web identity: the SHA-1 of the normalized, scheme-less URL (plan §2.8).
//!
//! [`normalize_desktop`] is a faithful port of the desktop `normalizeWebUrl`
//! (`electron/db.ts`), quirks included, so legacy `web:<sha1>` ids can be
//! reproduced ([`legacy_post_id`]). The web identity ([`from_url`]) applies the
//! same normalization and then drops the `http://` / `https://` scheme, so the
//! http and https forms of a site collapse into one item (this changes the
//! desktop behavior WEB-05 on purpose). Other schemes are kept, so they never
//! collide with a website.

use sha1::{Digest, Sha1};
use url::Url;

use super::{CanonicalId, IdError, Platform, to_hex};

/// Hex characters of the SHA-1 kept in the public key (`web_<sha1:20>`).
pub const KEY_HASH_HEX_LEN: usize = 20;

/// Query parameters dropped by the normalization (compared lowercase), besides
/// every `utm_*` parameter.
const TRACKING_PARAMS: [&str; 3] = ["gclid", "fbclid", "ref"];

/// Port of the desktop `normalizeWebUrl`: lowercase host without a leading
/// `www.`, no fragment, no tracking parameters, no trailing slash. A string
/// that does not parse as a URL is retried with `https://` in front; if that
/// fails too, the trimmed input is returned.
pub fn normalize_desktop(raw: &str) -> String {
    let parsed = Url::parse(raw).or_else(|_| Url::parse(&format!("https://{}", js_trim(raw))));
    let Ok(mut url) = parsed else {
        return js_trim(raw).to_owned();
    };

    // `u.hostname = u.hostname.toLowerCase().replace(/^www\./, '')`. The
    // setter ignores an invalid value (an empty host on a special scheme),
    // and so does this port.
    if let Some(host) = url.host_str() {
        let lowered = host.to_lowercase();
        let stripped = lowered.strip_prefix("www.").unwrap_or(&lowered);
        if stripped != host {
            let stripped = stripped.to_owned();
            let _ = url.set_host(Some(&stripped));
        }
    }
    url.set_fragment(None);

    // Deleting a parameter re-serializes the whole query as
    // application/x-www-form-urlencoded, which is what the url crate writes.
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let kept: Vec<&(String, String)> = pairs
            .iter()
            .filter(|(name, _)| !is_tracking_param(name))
            .collect();
        if kept.len() != pairs.len() {
            if kept.is_empty() {
                url.set_query(None);
            } else {
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(kept.iter().map(|(n, v)| (n.as_str(), v.as_str())));
            }
        }
    }

    strip_trailing_slash_like_desktop(url.as_str())
}

/// The desktop post id of a site: `web:` + SHA-1 of [`normalize_desktop`]
/// (`webPostId` in `electron/db.ts`).
pub fn legacy_post_id(raw: &str) -> String {
    format!("web:{}", sha1_hex(&normalize_desktop(raw)))
}

/// The scheme-less normalized URL hashed into the web identity.
pub fn normalize(raw: &str) -> String {
    let normalized = normalize_desktop(raw);
    match normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
    {
        Some(rest) => rest.to_owned(),
        None => normalized,
    }
}

/// The canonical identity of a site: `native_id` is the full SHA-1 (hex) of
/// [`normalize`], the key keeps its first [`KEY_HASH_HEX_LEN`] characters.
pub fn from_url(raw: &str) -> Result<CanonicalId, IdError> {
    if js_trim(raw).is_empty() {
        return Err(IdError::Empty);
    }
    let native_id = sha1_hex(&normalize(raw));
    let key_hash = native_id[..KEY_HASH_HEX_LEN].to_owned();
    Ok(CanonicalId::new(Platform::Web, native_id, &key_hash))
}

fn sha1_hex(value: &str) -> String {
    to_hex(Sha1::digest(value.as_bytes()).as_slice())
}

fn is_tracking_param(name: &str) -> bool {
    let lowered = name.to_lowercase();
    lowered.starts_with("utm_") || TRACKING_PARAMS.contains(&lowered.as_str())
}

/// The two trailing-slash rules of `normalizeWebUrl`, applied to the
/// serialized URL:
///
/// 1. `s.replace(/\/(?=$|\?)/, …)`: the first `/` followed by the end or by
///    `?` is removed unless the character before it is also a `/`;
/// 2. a remaining trailing `/` is removed unless the string is exactly
///    `http(s)://<host>/`.
fn strip_trailing_slash_like_desktop(serialized: &str) -> String {
    let bytes = serialized.as_bytes();
    let mut out = serialized.to_owned();
    let first = (0..bytes.len())
        .find(|&i| bytes[i] == b'/' && (i + 1 == bytes.len() || bytes[i + 1] == b'?'));
    if let Some(i) = first
        && !(i > 0 && bytes[i - 1] == b'/')
    {
        out.remove(i);
    }
    if out.ends_with('/') && !is_bare_http_origin(&out) {
        out.pop();
    }
    out
}

/// `^https?:\/\/[^/]+\/$`
fn is_bare_http_origin(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    match rest.strip_suffix('/') {
        Some(host) => !host.is_empty() && !host.contains('/'),
        None => false,
    }
}

/// `String.prototype.trim`: strips ECMAScript white space and line terminators,
/// which differ from Rust's `char::is_whitespace` (U+FEFF, U+0085).
fn js_trim(value: &str) -> &str {
    value.trim_matches(|c: char| {
        matches!(
            c,
            '\u{0009}'..='\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// `(input, normalizeWebUrl(input), webPostId(input))`, produced by running
    /// the real functions exported by `electron/db.ts` under Node 22 (tsx) on
    /// synthetic inputs.
    #[rustfmt::skip]
    const DESKTOP_VECTORS: &[(&str, &str, &str)] = &[
        ("https://example.com", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("https://example.com/", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("http://example.com/", "http://example.com", "web:89dce6a446a69d6b9bdc01ac75251e4c322bcdff"),
        ("HTTPS://WWW.Example.COM/Path/?utm_source=x&b=2#frag", "https://example.com/Path?b=2", "web:9a1c2c285372a14fe15c878a4b3bba9090202b27"),
        ("example.com", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("www.example.com/path/", "https://example.com/path", "web:abc4f9ef98929270793482fb61ffe0e6cf419e39"),
        ("https://example.com/a/b/", "https://example.com/a/b", "web:6bc63113c8dea5d72549ac9952bfda60c955b2d3"),
        ("https://example.com/a//", "https://example.com/a/", "web:215950a8a24fd8d02c1afa43379400f574308fe6"),
        ("https://example.com/?q=1", "https://example.com?q=1", "web:f6123532d1bc02bb9c68a1c07410e93ba7274edc"),
        ("https://example.com/a?redirect=/b/?c", "https://example.com/a?redirect=/b?c", "web:a346237a1c44753f51b8a70e04835df19e215071"),
        ("https://example.com/a?x=/", "https://example.com/a?x=", "web:fa720443f46eaf46ed7d6d8a15c92ca37cc2494f"),
        ("https://example.com/?utm_source=a&utm_medium=b", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("https://example.com/p?ref=abc&id=5&fbclid=zz&GCLID=1", "https://example.com/p?id=5", "web:cd4a14bb41c59e536d623187935d96f120419a90"),
        ("https://example.com/p?UTM_Campaign=spring&q=a+b&z=%20c", "https://example.com/p?q=a+b&z=+c", "web:73db3d606a4ee79fdf98cdcca26bc6984be5eae1"),
        ("https://example.com/p?q=a%2Bb&ref=1", "https://example.com/p?q=a%2Bb", "web:ddbb84b03f3c2fbd55b6c8910af66c820ffc8ba7"),
        ("https://example.com/p?q=caf%C3%A9&ref=1", "https://example.com/p?q=caf%C3%A9", "web:7a9c9b01eb3894c0c5dd4518e2a98ab9bf2e4f8a"),
        ("https://example.com/p?flag&utm_x=1", "https://example.com/p?flag=", "web:f9e062efd0d8e57f7275d3adb5ce89b2c39e839b"),
        ("https://example.com/p?q=~tilde&ref=1", "https://example.com/p?q=%7Etilde", "web:66783b9cca049bb9b752875da9ca892dfdfddf1f"),
        ("https://example.com/p?q=~tilde", "https://example.com/p?q=~tilde", "web:208ce3454c02652d88c5c9fb61fedb25e2b3cf82"),
        ("https://bücher.example/straße", "https://xn--bcher-kva.example/stra%C3%9Fe", "web:60fa20cb8a7157a896038f76a8a57a61175b1c74"),
        ("https://example.com:443/x", "https://example.com/x", "web:4701cd48f5015b44043f92428b11b2ffae394c27"),
        ("http://example.com:8080/x/", "http://example.com:8080/x", "web:245d996b4528cea7faa657ba0a3298a2eae05674"),
        ("  https://example.com/x  ", "https://example.com/x", "web:4701cd48f5015b44043f92428b11b2ffae394c27"),
        ("https://www.example.com.", "https://example.com.", "web:95d9f7b7c3ca581347a197d9cc2b601415b8f787"),
        ("https://user:pass@www.example.com/x", "https://user:pass@example.com/x", "web:e7216b5b8e96164211e4b3548c14b0f25cf17d37"),
        ("ftp://www.example.com/x/", "ftp://example.com/x", "web:9fb74c00227b832e8e12472791ffc6343ad1f905"),
        ("mailto:someone@example.com", "mailto:someone@example.com", "web:5a9db2ee430912e7250da417e3a5554a47f79845"),
        ("not a url", "not a url", "web:d7aad9a0157a961b97833ff47c0d06f7add5a336"),
        ("", "", "web:da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        ("https://example.com/index.html?", "https://example.com/index.html?", "web:14fff294a4f51dee4ad40deadca64f91598d3d1c"),
        ("https://example.com/?", "https://example.com?", "web:33928b1063b8de57d9997e75c5b677d960ad6973"),
        ("https://example.com/#top", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("https://example.com/a b", "https://example.com/a%20b", "web:3931b4baf9a2f2b4b0093f9ba3c2580467faa6bf"),
        ("HTTP://EXAMPLE.COM/A/", "http://example.com/A", "web:aeb915f0d87b69d59fc21e53dc28133136e87e1f"),
        ("https://www.www.example.com/", "https://www.example.com", "web:740e7397907c0b004010d92b33d283e98f74063d"),
        ("https://example.com/p?utm_source=x", "https://example.com/p", "web:0eb4189a82716854155a553676b237c9d1c47dab"),
        ("https://example.com/p/?utm_source=x", "https://example.com/p", "web:0eb4189a82716854155a553676b237c9d1c47dab"),
        ("https://example.com?utm_source=x", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("www.example.com:8080", "www.example.com:8080", "web:a6f193188a09f9e22afa8e20d533bba622b00fbe"),
        ("https://xn--bcher-kva.example/", "https://xn--bcher-kva.example", "web:d29f979bafdad670d7adcc042e21528097a3756b"),
        ("https://EXAMPLE.com/%7Euser/", "https://example.com/%7Euser", "web:1e581ff6f91e9812fb0bc8983475255920eb2ba6"),
        ("https://example.com/a/?b=1/", "https://example.com/a?b=1", "web:8849d9f8cb0cc1d03f623d5c28e58e0649193878"),
        ("https://example.com//", "https://example.com/", "web:b559c7edd3fb67374c1a25e739cdd7edd1d79949"),
        ("http://localhost:3000/app/", "http://localhost:3000/app", "web:257bdc8b834760ed30c790f85fc6a4217c4752d6"),
        ("https://192.168.0.1/x", "https://192.168.0.1/x", "web:2f57e07713fcd2bb6a3ca32bc16520a532875ee2"),
        ("https://[::1]/x/", "https://[::1]/x", "web:7ded30ec2af64d9ce517be52b6909099d9c9d439"),
        ("https://exa\tmple.com/", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("https://example.com/p?a=1&a=2&utm_source=x", "https://example.com/p?a=1&a=2", "web:b3d3649ef7ee7037e2d85b285d5be79f2579566c"),
        ("https://example.com/p?a=1;utm_source=x", "https://example.com/p?a=1;utm_source=x", "web:bc860f47cd97c79a7f386758b0c146199f0fd973"),
        ("https://example.com/p?Ref=1", "https://example.com/p", "web:0eb4189a82716854155a553676b237c9d1c47dab"),
        (" www.example.org/x ", "https://example.org/x", "web:295ad4833399b0adcd4eb659b765d63ab61b1152"),
        ("\u{FEFF}example.org", "https://example.org", "web:434178b2be512144473ad9f2eb59f4aa9768407c"),
        ("https://example.com/p?q=%zz&ref=1", "https://example.com/p?q=%25zz", "web:b4265e6d04c837a45e158e84146419bac84f57aa"),
        ("https://example.com/p?q=a&&ref=1&", "https://example.com/p?q=a", "web:6ec50050857d0031b48b97849d1ce5ae88edafcf"),
        ("https://example.com/p?=v&ref=1", "https://example.com/p?=v", "web:39ea5868b13f2eb9189bb030019d59c4976c443e"),
        ("https://example.com/p?a=%F0%9F%98%80&utm_term=x", "https://example.com/p?a=%F0%9F%98%80", "web:6712d9f8eba149041399ec721c2cb4bb3775b2fc"),
        ("https://example.com/p?a=b+c&utm_term=x", "https://example.com/p?a=b+c", "web:9d753333fc5372a0e7c1b492122f8477e13270f0"),
        ("https://example.com/p?a=*-._!'()&utm_term=x", "https://example.com/p?a=*-._%21%27%28%29", "web:38ff837709f5cd8b696366360959aa58d6e10f1c"),
        ("http://www./x", "http://www./x", "web:46f83b39579f7ffb0fd18b03c4a1dfb4d76b06ec"),
        ("https://WWW.EXAMPLE.COM", "https://example.com", "web:327c3fda87ce286848a574982ddd0b7c7487f816"),
        ("example.com/Foo/?utm_medium=y#bar", "https://example.com/Foo", "web:d315300c1e593675554c1b2aa6024cdb5a21d847"),
        ("https://example.com/a/b/c.html#x", "https://example.com/a/b/c.html", "web:79d5835bda3c732cbf2004a80f55f8eb6439e49e"),
        ("https://example.com/;params/", "https://example.com/;params", "web:3500e97df439082784c95c31b3fbbb4d1d54212e"),
    ];

    #[test]
    fn normalization_matches_the_desktop() {
        for &(input, normalized, legacy_id) in DESKTOP_VECTORS {
            assert_eq!(
                normalize_desktop(input),
                normalized,
                "normalizeWebUrl({input:?})"
            );
            assert_eq!(legacy_post_id(input), legacy_id, "webPostId({input:?})");
        }
    }

    #[test]
    fn http_and_https_collapse() {
        let https = from_url("https://www.example.com/work/").unwrap();
        let http = from_url("http://example.com/work").unwrap();
        let bare = from_url("example.com/work/?utm_source=newsletter").unwrap();
        assert_eq!(https, http);
        assert_eq!(https, bare);
        assert_eq!(
            normalize("https://www.example.com/work/"),
            "example.com/work"
        );
        assert_eq!(https.platform(), Platform::Web);
        assert_eq!(https.native_id().len(), 40);
        assert_eq!(
            https.key(),
            format!("web_{}", &https.native_id()[..KEY_HASH_HEX_LEN])
        );
        // The desktop kept the two schemes apart.
        assert_ne!(
            legacy_post_id("https://example.com/work"),
            legacy_post_id("http://example.com/work")
        );
    }

    #[test]
    fn known_web_keys() {
        // sha1("example.com")
        let id = from_url("https://example.com/").unwrap();
        assert_eq!(id.native_id(), "0caaf24ab1a0c33440c06afe99df986365b0781f");
        assert_eq!(id.key(), "web_0caaf24ab1a0c33440c0");
    }

    #[test]
    fn other_schemes_and_paths_stay_distinct() {
        assert_ne!(
            from_url("ftp://example.com/x").unwrap(),
            from_url("https://example.com/x").unwrap()
        );
        assert_ne!(
            from_url("https://example.com/a").unwrap(),
            from_url("https://example.com/b").unwrap()
        );
        assert_ne!(
            from_url("https://example.com/A").unwrap(),
            from_url("https://example.com/a").unwrap()
        );
    }

    #[test]
    fn empty_urls_have_no_identity() {
        assert_eq!(from_url(""), Err(IdError::Empty));
        assert_eq!(from_url(" \u{FEFF} "), Err(IdError::Empty));
    }

    proptest! {
        #[test]
        fn scheme_never_changes_the_identity(
            host in "h[a-z0-9-]{0,20}\\.(com|org|it)",
            path in "(/[a-zA-Z0-9._~-]{1,10}){0,4}/?",
            www in proptest::bool::ANY,
        ) {
            let www = if www { "www." } else { "" };
            let https = from_url(&format!("https://{www}{host}{path}")).unwrap();
            let http = from_url(&format!("http://{host}{path}")).unwrap();
            prop_assert_eq!(https, http);
        }

        #[test]
        fn normalization_is_idempotent_on_its_output(
            host in "h[a-z0-9-]{0,20}\\.(com|org|it)",
            path in "(/[a-zA-Z0-9._~-]{1,10}){0,4}/?",
            query in "(\\?[a-z]{1,5}=[a-z0-9]{0,5})?",
        ) {
            let once = normalize_desktop(&format!("https://{host}{path}{query}"));
            prop_assert_eq!(normalize_desktop(&once), once.clone());
        }
    }
}
