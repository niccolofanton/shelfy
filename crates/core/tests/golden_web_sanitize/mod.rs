use super::check;
use serde_json::Value;
#[test]
fn website_sanitizer_matches_desktop_bytes() {
    check("ai/web-sanitize", |(value, max): (Value, usize)| {
        shelfy_core::ai::sanitize::for_prompt(&value, max)
    });
}
