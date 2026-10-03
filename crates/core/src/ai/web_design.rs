//! Desktop design catalog v2 mapping. Evidence and every measured facet survive
//! in ai_web_json; legacy columns remain searchable by the existing API.
use super::prompts::{self, Task};
use crate::repo::posts::AiPatch;
use serde_json::{Value, json};
fn strn(v: &Value, max: usize) -> String {
    let s = v.as_str().map(crate::search::terms::js_trim).unwrap_or("");
    let mut units = 0;
    s.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max
        })
        .collect()
}
fn unique(v: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = vec![];
    for s in v {
        if !out.contains(&s) {
            out.push(s)
        }
    }
    out
}
fn arr(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}
fn list(v: &Value, allowed: Option<&Value>, max: usize) -> Vec<String> {
    let values = arr(v)
        .filter_map(Value::as_str)
        .map(crate::search::terms::js_trim)
        .filter(|s| !s.is_empty())
        .filter(|s| allowed.is_none_or(|a| arr(a).any(|v| v.as_str() == Some(s))))
        .map(String::from);
    unique(values).into_iter().take(max).collect()
}
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn js(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(a) => a
            .iter()
            .map(|x| if x.is_null() { String::new() } else { js(x) })
            .collect::<Vec<_>>()
            .join(","),
        _ => v.to_string(),
    }
}
/// `post` has the desktop's webMeta/webFonts/webPalette/webAwards fields.
pub fn map(raw: &Value, post: &Value, model: &str) -> Value {
    let schema = prompts::response_schema(Task::WebDesign).expect("design schema");
    let props = &schema.value["properties"];
    let one = |key: &str, fallback: &str| {
        let s = strn(&raw[key], 80);
        if arr(&props[key]["enum"]).any(|v| v.as_str() == Some(&s)) {
            s
        } else {
            fallback.into()
        }
    };
    let many = |key: &str, max| list(&raw[key], Some(&props[key]["items"]["enum"]), max);
    let meta = &post["webMeta"];
    let site_type = one("site_type", "other");
    let secondary = one("site_type_secondary", "none");
    let industry = one("industry", "other");
    let style = many("style", 3);
    let scheme = if truth(&meta["scheme"]) {
        js(&meta["scheme"])
    } else {
        "light".into()
    };
    let theme = one("theme", &scheme);
    let color = many("color_mood", 3);
    let density = one("density", "balanced");
    let layout = many("layout_patterns", 5);
    let hero = one("hero_type", "other");
    let imagery = many("imagery", 3);
    let fonts = arr(&post["webFonts"]).collect::<Vec<_>>();
    let classes = unique(
        fonts
            .iter()
            .filter_map(|v| v["classification"].as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
    );
    let has = |class: &str| classes.iter().any(|s| s == class);
    let typography = many("typography", 3)
        .into_iter()
        .filter(|t| {
            fonts.is_empty()
                || match t.as_str() {
                    "monospace accents" => has("mono"),
                    "serif editorial" | "mixed serif-sans" => has("serif"),
                    "script accents" => has("script"),
                    _ => true,
                }
        })
        .collect::<Vec<_>>();
    let components = many("components", 6);
    let craft = one("craft", "solid");
    let tags = list(&raw["tags"], None, 8)
        .into_iter()
        .map(|s| {
            let lower = s.to_lowercase();
            lower.strip_prefix('#').unwrap_or(&lower).to_string()
        })
        .collect::<Vec<_>>();
    let summary = strn(&raw["summary"], 400);
    let description = strn(&raw["description"], 900);
    let notable = list(&raw["notable_details"], None, 3);
    let references = list(&raw["reference_for"], None, 3);
    let keywords = list(&raw["search_keywords"], None, 6);
    let tech = arr(&meta["tech"])
        .filter(|v| v["confidence"].as_f64().is_some_and(|c| c >= 0.8))
        .filter_map(|v| v["name"].as_str())
        .map(String::from)
        .collect::<Vec<_>>();
    let font_names = fonts
        .iter()
        .filter_map(|v| v["family"].as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    let traits = &meta["traits"];
    let motion = [
        ("smoothScroll", "smooth-scroll"),
        ("scrollJacked", "scroll-jacked"),
        ("webgl", "webgl"),
        ("pageTransitions", "page-transitions"),
        ("marquee", "marquee"),
        ("customCursor", "custom-cursor"),
        ("videoBackground", "video-hero"),
        ("glass", "glass"),
    ]
    .into_iter()
    .filter(|(key, _)| truth(&traits[*key]))
    .map(|(_, name)| name)
    .collect::<Vec<_>>();
    let palette = unique(
        arr(&post["webPalette"])
            .filter(|v| matches!(v["role"].as_str(), Some("accent" | "background")))
            .filter_map(|v| v["name"].as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
    );
    let awards = arr(&post["webAwards"])
        .map(|v| v["platform"].clone())
        .collect::<Vec<_>>();
    let language = if truth(&meta["lang"]) {
        js(&meta["lang"])
    } else {
        String::new()
    };
    let facets = json!({"siteType":if secondary=="none"{vec![site_type.clone()]}else{vec![site_type.clone(),secondary.clone()]},"industry":[industry],"style":style,"theme":[theme],"colorMood":color,"density":[density],"layout":layout,"hero":[hero],"imagery":imagery,"typography":typography,"components":components,"craft":[craft],"tech":tech,"font":font_names,"fontClass":classes,"color":palette,"scheme":if truth(&meta["scheme"]){vec![js(&meta["scheme"])]}else{vec![]},"motion":motion,"award":awards});
    let catalog = json!({"schema":2,"model":model,"observations":strn(&raw["observations"],1200),"siteType":site_type,"siteTypeSecondary":if secondary=="none"{Value::Null}else{json!(secondary)},"industry":industry,"audience":strn(&raw["audience"],120),"style":style,"theme":theme,"colorMood":color,"density":density,"layoutPatterns":layout,"heroType":hero,"imagery":imagery,"typography":typography,"components":components,"craft":craft,"notableDetails":notable,"referenceFor":references,"summary":summary,"description":description,"tags":tags,"searchKeywords":keywords,"language":language,"facets":facets});
    let general = vec![
        site_type,
        industry,
        if theme == "dark" {
            "dark mode".into()
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>();
    let specific = unique(
        style
            .into_iter()
            .chain(layout.into_iter().take(3))
            .chain(typography.into_iter().take(2))
            .chain(tags),
    )
    .into_iter()
    .take(14)
    .collect::<Vec<_>>();
    let entities = unique(
        [
            if truth(&meta["siteName"]) {
                js(&meta["siteName"])
            } else {
                String::new()
            },
            meta["organization"]["name"].as_str().unwrap_or("").into(),
        ]
        .into_iter()
        .chain(font_names)
        .chain(tech)
        .chain(
            arr(&meta["awardEntities"])
                .filter_map(Value::as_str)
                .map(String::from),
        )
        .filter(|s| !s.is_empty() && s.encode_utf16().count() < 80),
    );
    json!({"catalog":catalog,"description":([summary.clone(),description].into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join("\n\n")),"saveReason":if references.is_empty(){summary}else{references.join(" · ")},"tags":unique(general.iter().chain(specific.iter()).cloned()),"generalTags":general,"specificTags":specific,"entities":entities,"keywords":keywords,"category":catalog["industry"],"contentType":catalog["siteType"],"language":if language.is_empty(){"en".into()}else{language}})
}
pub fn patch(mapped: &Value, provider: &str, model: &str) -> AiPatch {
    let strings = |key: &str| {
        arr(&mapped[key])
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    };
    let text = |key: &str| Some(Some(mapped[key].as_str().unwrap_or("").into()));
    AiPatch {
        status: Some(Some("done".into())),
        provider: Some(Some(provider.into())),
        model: Some(Some(model.into())),
        schema_version: Some(Some(2)),
        error: Some(None),
        description: text("description"),
        tags: Some(Some(strings("tags"))),
        general_tags: Some(strings("generalTags")),
        specific_tags: Some(strings("specificTags")),
        category: text("category"),
        content_type: text("contentType"),
        entities: Some(Some(strings("entities"))),
        keywords: Some(Some(strings("keywords"))),
        language: text("language"),
        save_reason: text("saveReason"),
        web: Some(Some(mapped["catalog"].clone())),
        ..Default::default()
    }
}

/// Measured evidence used by the curator. Values come from capture probes,
/// never visual guesses; strings are inert and budgeted before prompting.
pub fn ground(post: &Value) -> Value {
    fn inert(v: &Value, depth: usize) -> Value {
        if depth > 8 {
            return Value::Null;
        }
        match v {
            Value::String(s) => json!(super::sanitize::text(s, 400)),
            Value::Array(a) => {
                Value::Array(a.iter().take(30).map(|v| inert(v, depth + 1)).collect())
            }
            Value::Object(o) => Value::Object(
                o.iter()
                    .take(40)
                    .map(|(k, v)| (k.clone(), inert(v, depth + 1)))
                    .collect(),
            ),
            _ => v.clone(),
        }
    }
    let meta = &post["webMeta"];
    let prefix = |key: &str, max: usize| {
        Value::Array(arr(&post[key]).take(max).map(|v| inert(v, 0)).collect())
    };
    json!({"site":post["authorName"],"domain":post["webDomain"],"languages":inert(&json!([meta["lang"],meta["languages"]]),0),"colorScheme":meta["scheme"],"palette":prefix("webPalette",10),"fonts":prefix("webFonts",6),"technologies":inert(&meta["tech"],0),"motionAndLayoutFacts":inert(&meta["traits"],0),"awards":prefix("webAwards",30),"textContrast":inert(&meta["contrast"],0),"baseSize":meta["baseSize"],"scaleRatio":meta["scaleRatio"],"typeScale":inert(&meta["typeScale"],0),"jsonldTypes":inert(&meta["jsonldTypes"],0),"organization":inert(&meta["organization"],0),"scrollVideoRecorded":truth(&meta["video"]),"siteName":inert(&meta["siteName"],0)})
}

/// Provider output must satisfy the complete rich schema before persistence.
pub fn valid(raw: &Value) -> bool {
    static VALIDATOR: std::sync::LazyLock<jsonschema::Validator> = std::sync::LazyLock::new(|| {
        jsonschema::validator_for(
            &prompts::response_schema(Task::WebDesign)
                .expect("design schema")
                .value,
        )
        .expect("design schema compiles")
    });
    VALIDATOR.is_valid(raw)
}
