//! Colour math of a captured site's palette (plan §2.14, WEB-45; P4-05): the
//! OKLab conversion and the "same colour" distance the `color` filter and the
//! `similar` tie-break use.
//!
//! Ported byte for byte from the desktop's `electron/webcap/metadata.ts`
//! (`hexToLab`, `rgbToOklab`) and the palette distance in its
//! `electron/db.ts` (`swatchDistance`): same constants, same operations, same
//! order, so a golden fixture built from the TypeScript (`scripts/golden/
//! web-sites.ts`) matches this module's output (`crates/core/tests/
//! web_sites.rs`).
//!
//! A captured site's palette (`web_captures.palette_json`) is a JSON array of
//! swatches, each `{ hex, role, … }` (role one of `background`, `surface`,
//! `text`, `accent`, `image`), or, for a migrated v1 site, a bare hex string
//! with no role. [`palette_distance`] and [`tie_break_swatches`] read either
//! shape, as the desktop's `rowToPost` does (`typeof c === 'string' ? { hex: c
//! } : c`).

use serde_json::Value;

/// A colour in OKLab space: `l` is lightness, `a` and `b` the two chroma axes
/// (the desktop's `Lab = [number, number, number]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    /// Lightness.
    pub l: f64,
    /// Green–red axis.
    pub a: f64,
    /// Blue–yellow axis.
    pub b: f64,
}

/// The desktop's cutoff for "the same colour" (`swatchDistance`'s `d <= 1`):
/// at most this far, in [`swatch_distance`] units, from a target.
pub const MAX_MATCH_DISTANCE: f64 = 1.0;

/// How many of a site's own background/accent swatches feed the `similar`
/// palette-proximity tie-break (the desktop's `mineSw.slice(0, 3)`).
const TIE_BREAK_SWATCHES: usize = 3;

/// sRGB channel (0–255) to linear light (the desktop's `srgbToLinear`).
fn srgb_to_linear(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB to OKLab (the desktop's `rgbToOklab`): linearize, the LMS cube roots,
/// then the OKLab matrix.
#[must_use]
pub fn rgb_to_oklab(r: u8, g: u8, b: u8) -> Lab {
    let lr = srgb_to_linear(r);
    let lg = srgb_to_linear(g);
    let lb = srgb_to_linear(b);
    let l = (0.412_221_470_8 * lr + 0.536_332_536_3 * lg + 0.051_445_992_9 * lb).cbrt();
    let m = (0.211_903_498_2 * lr + 0.680_699_545_1 * lg + 0.107_396_956_6 * lb).cbrt();
    let s = (0.088_302_461_9 * lr + 0.281_718_837_6 * lg + 0.629_978_700_5 * lb).cbrt();
    Lab {
        l: 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        a: 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        b: 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    }
}

/// Parses a `#rrggbb` (or `rrggbb`) hex colour into OKLab (the desktop's
/// `hexToLab`). `None` for anything else: a short or long hex, a non-hex
/// character, or an alpha channel. Matches `/^#?([0-9a-f]{6})$/i` on the
/// trimmed input, case-insensitively.
#[must_use]
pub fn hex_to_lab(hex: &str) -> Option<Lab> {
    let trimmed = hex.trim();
    let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if digits.len() != 6 || !digits.is_ascii() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(digits, 16).ok()?;
    #[allow(clippy::cast_possible_truncation)] // masked to a byte first
    let byte = |shift: u32| ((n >> shift) & 0xff) as u8;
    Some(rgb_to_oklab(byte(16), byte(8), byte(0)))
}

/// The desktop's per-swatch distance (`swatchDistance`'s inner formula):
/// lightness drift tolerated more than hue/chroma drift, normalised so `1.0`
/// reads as "a designer calls this the same colour".
fn swatch_distance(a: Lab, target: Lab) -> f64 {
    let lightness = (a.l - target.l).abs() / 0.15;
    let chroma = (a.a - target.a).hypot(a.b - target.b) / 0.06;
    lightness.max(chroma)
}

/// One swatch's `hex` and `role`, reading both the v2 shape (`{hex, role,
/// …}`) and a migrated v1 bare hex string (role `None`); `None` when `item`
/// is neither (untrusted capture or AI output, §2.18 lane rule 4).
fn hex_and_role(item: &Value) -> Option<(&str, Option<&str>)> {
    match item {
        Value::String(hex) => Some((hex.as_str(), None)),
        Value::Object(map) => Some((
            map.get("hex")?.as_str()?,
            map.get("role").and_then(Value::as_str),
        )),
        _ => None,
    }
}

/// Whether a swatch role is excluded from colour matching: the desktop's
/// `swatchDistance` skips `text` and `image` swatches (everything else,
/// `background`, `surface`, `accent`, and an absent role on a v1 swatch,
/// counts).
fn is_excluded_role(role: Option<&str>) -> bool {
    matches!(role, Some("text" | "image"))
}

/// The distance from `target` to the closest eligible swatch of `palette`
/// (`web_captures.palette_json`, as stored): the desktop's `swatchDistance`.
/// `None` when `palette` is not an array or has no eligible, parseable
/// swatch (the desktop's `Infinity`, which a `d <= 1` comparison always
/// fails) — compose with [`MAX_MATCH_DISTANCE`] via
/// `palette_distance(..).is_some_and(|d| d <= MAX_MATCH_DISTANCE)`.
#[must_use]
pub fn palette_distance(palette: &Value, target: Lab) -> Option<f64> {
    palette.as_array()?.iter().fold(None, |best, item| {
        let Some((hex, role)) = hex_and_role(item) else {
            return best;
        };
        if is_excluded_role(role) {
            return best;
        }
        let Some(lab) = hex_to_lab(hex) else {
            return best;
        };
        let d = swatch_distance(lab, target);
        Some(best.map_or(d, |b: f64| b.min(d)))
    })
}

/// The OKLab of up to [`TIE_BREAK_SWATCHES`] of a site's own `background` or
/// `accent` swatches, in palette order (the desktop's `similarWebReferences`
/// tie-break: `mineSw`). The window is fixed to the first matching swatches;
/// one that fails to parse as a hex colour is dropped, not replaced by a
/// later one (mirrors the desktop's `.slice(0, 3)` before `.map(hexToLab)`).
#[must_use]
pub fn tie_break_swatches(palette: &Value) -> Vec<Lab> {
    let Some(items) = palette.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(hex_and_role)
        .filter(|(_, role)| matches!(role, Some("background" | "accent")))
        .take(TIE_BREAK_SWATCHES)
        .filter_map(|(hex, _)| hex_to_lab(hex))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hex_to_lab_accepts_the_desktops_shapes() {
        assert!(hex_to_lab("#533afd").is_some());
        assert!(hex_to_lab("533AFD").is_some(), "no hash, uppercase");
        assert!(hex_to_lab("  #533afd  ").is_some(), "trimmed");
        assert!(hex_to_lab("#53afd").is_none(), "too short");
        assert!(hex_to_lab("#533afd1").is_none(), "too long");
        assert!(hex_to_lab("#533afg").is_none(), "not hex");
        assert!(hex_to_lab("#533afd12").is_none(), "alpha channel");
        assert!(hex_to_lab("").is_none());
    }

    #[test]
    fn rgb_to_oklab_round_trips_known_black_and_white() {
        let black = rgb_to_oklab(0, 0, 0);
        assert!((black.l).abs() < 1e-9);
        assert!((black.a).abs() < 1e-9);
        assert!((black.b).abs() < 1e-9);
        let white = rgb_to_oklab(255, 255, 255);
        assert!((white.l - 1.0).abs() < 1e-6, "L of white is ~1: {white:?}");
        assert!(
            white.a.abs() < 1e-6 && white.b.abs() < 1e-6,
            "white is neutral: {white:?}"
        );
    }

    #[test]
    fn palette_distance_skips_text_and_image_swatches() {
        let target = hex_to_lab("#ffffff").unwrap();
        // A near-white "text" swatch must not win over a mid-grey eligible one.
        let palette = json!([
            { "hex": "#fefefe", "role": "text" },
            { "hex": "#808080", "role": "surface" },
        ]);
        let d = palette_distance(&palette, target).unwrap();
        let surface_only = json!([{ "hex": "#808080", "role": "surface" }]);
        assert_eq!(d, palette_distance(&surface_only, target).unwrap());
    }

    #[test]
    fn palette_distance_reads_bare_v1_hex_strings() {
        let target = hex_to_lab("#000000").unwrap();
        let palette = json!(["#010101"]);
        assert!(palette_distance(&palette, target).unwrap() < MAX_MATCH_DISTANCE);
    }

    #[test]
    fn palette_distance_is_none_without_an_eligible_swatch() {
        let target = hex_to_lab("#000000").unwrap();
        assert_eq!(palette_distance(&json!([]), target), None);
        assert_eq!(palette_distance(&json!(null), target), None);
        assert_eq!(
            palette_distance(&json!([{ "hex": "#fff", "role": "text" }]), target),
            None,
            "invalid hex (3 digits) is skipped, leaving no eligible swatch"
        );
        assert_eq!(
            palette_distance(&json!([{ "role": "background" }]), target),
            None,
            "a swatch without hex is skipped"
        );
    }

    #[test]
    fn tie_break_swatches_is_an_allowlist_of_background_and_accent() {
        let palette = json!([
            { "hex": "#111111", "role": "text" },
            { "hex": "#222222", "role": "background" },
            { "hex": "#333333", "role": "surface" },
            { "hex": "#444444", "role": "accent" },
            { "hex": "#555555", "role": "accent" },
        ]);
        let labs = tie_break_swatches(&palette);
        assert_eq!(
            labs,
            vec![
                hex_to_lab("#222222").unwrap(),
                hex_to_lab("#444444").unwrap(),
                hex_to_lab("#555555").unwrap(),
            ],
            "surface and text are excluded; the 3 remaining background/accent swatches all fit the window"
        );
    }

    #[test]
    fn tie_break_swatches_window_is_fixed_not_backfilled() {
        // Of the first 3 background/accent swatches, the middle one is
        // unparseable: the result has 2 entries, not 3 (the 4th candidate
        // never enters the window).
        let palette = json!([
            { "hex": "#111111", "role": "background" },
            { "hex": "not-a-colour", "role": "accent" },
            { "hex": "#333333", "role": "accent" },
            { "hex": "#444444", "role": "background" },
        ]);
        let labs = tie_break_swatches(&palette);
        assert_eq!(
            labs,
            vec![
                hex_to_lab("#111111").unwrap(),
                hex_to_lab("#333333").unwrap()
            ]
        );
    }
}
