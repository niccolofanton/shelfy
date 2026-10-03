//! The desktop web-enrich sanitizer: markup and prompt delimiters become inert
//! text before a site's digest is sent to a provider.
use crate::search::terms::js_trim;
use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::LazyLock;
static MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<<<\s*/?\s*FINE\s+CAPTION\s*>>>|<<<\s*CAPTION\s*>>>").unwrap()
});
static ANGLES_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<{3,}").unwrap());
static ANGLES_OUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r">{3,}").unwrap());
static COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->|<!\[CDATA\[.*?\]\]>").unwrap());
static ENTITIES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)&(#x?[0-9a-f]+|[a-z]+);").unwrap());
fn markers(s: &str) -> String {
    ANGLES_OUT
        .replace_all(
            &ANGLES_IN.replace_all(&MARKERS.replace_all(s, " "), "‹"),
            "›",
        )
        .into_owned()
}
fn string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.iter().map(string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn decode(s: &str) -> String {
    ENTITIES
        .replace_all(s, |c: &Captures<'_>| {
            let body = &c[1];
            if let Some(body) = body.strip_prefix('#') {
                let (text, base) =
                    if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
                        (hex, 16)
                    } else {
                        (body, 10)
                    };
                // JS parseInt accepts a decimal prefix before a hex letter.
                let digits = text
                    .chars()
                    .take_while(|c| c.is_digit(base))
                    .collect::<String>();
                return u32::from_str_radix(&digits, base)
                    .ok()
                    .filter(|n| *n > 0)
                    .and_then(char::from_u32)
                    .map(|ch| ch.to_string())
                    .unwrap_or_else(|| c[0].into());
            }
            match body.to_ascii_lowercase().as_str() {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                "nbsp" => " ",
                "copy" => "©",
                "reg" => "®",
                "trade" => "™",
                "hellip" => "…",
                "mdash" => "—",
                "ndash" => "–",
                "rsquo" => "’",
                "lsquo" => "‘",
                "rdquo" => "”",
                "ldquo" => "“",
                "deg" => "°",
                "euro" => "€",
                "pound" => "£",
                "laquo" => "«",
                "raquo" => "»",
                _ => &c[0],
            }
            .to_owned()
        })
        .into_owned()
}
fn strip(s: &str) -> String {
    let clean = COMMENTS.replace_all(s, " ");
    let mut out = String::new();
    let mut rest = clean.as_ref();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        out.push(' ');
        let Some(gt) = rest[lt + 1..].find('>') else {
            return out;
        };
        rest = &rest[lt + 1 + gt + 1..];
    }
    out.push_str(rest);
    out
}
/// JS-compatible sanitizer on JSON-shaped input. `max_chars=0` uses the
/// desktop default (1,200 UTF-16 units); the hard ceiling is 20,000.
pub fn for_prompt(value: &Value, max_chars: usize) -> String {
    text(&string(value), max_chars)
}
/// String input, with the same delimiter, entity, whitespace and word cap.
pub fn text(value: &str, max_chars: usize) -> String {
    let max = if max_chars == 0 {
        1200
    } else {
        max_chars.clamp(1, 20_000)
    };
    let neutral = markers(&decode(&strip(&markers(value))));
    let chars = neutral
        .chars()
        .filter_map(|ch| match ch as u32 {
            0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x206a..=0x206f
            | 0xfeff
            | 0xad => None,
            0..=8 | 11..=12 | 14..=31 | 127..=159 => Some(' '),
            _ => Some(ch),
        })
        .collect::<String>();
    let mut out = String::new();
    let mut space = false;
    for ch in chars.chars() {
        if crate::search::terms::js_trim(&ch.to_string()).is_empty() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(ch);
        }
    }
    if out.encode_utf16().count() > max {
        let mut units = 0;
        let cut = out
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units <= max
            })
            .collect::<String>();
        let last = cut.rfind(' ');
        let cut = if last.is_some_and(|i| cut[..i].encode_utf16().count() as f64 > max as f64 * 0.6)
        {
            &cut[..last.unwrap()]
        } else {
            &cut
        };
        out = format!("{}…", js_trim(cut));
    }
    out
}
