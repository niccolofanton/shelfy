//! Search-chat system prompt from shared/ai, byte-for-byte desktop parity.
use super::{
    chat::{MAX_KEYWORDS, PER_TIER_CAP},
    prompts::{self, PromptError, Task},
    template::Var,
};
use crate::search::terms::js_trim;
/// Offers only real archive tag pools and the active refinement context.
/// Display strings are preserved like the desktop; blank members are omitted.
pub fn system(
    broad: &[String],
    specific: &[String],
    active: &[String],
) -> Result<String, PromptError> {
    let join = |values: &[String]| {
        values
            .iter()
            .filter(|t| !js_trim(t).is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let broad = join(broad);
    let specific = join(specific);
    let active = join(active);
    let tier = PER_TIER_CAP.to_string();
    let keywords = MAX_KEYWORDS.to_string();
    prompts::system_prompt(
        Task::Chat,
        &[
            ("broad", Var::Text(&broad)),
            ("specific", Var::Text(&specific)),
            ("active", Var::Text(&active)),
            ("perTierCap", Var::Text(&tier)),
            ("maxKeywords", Var::Text(&keywords)),
            ("generalOpen", Var::Text("[[GENERAL]]")),
            ("generalClose", Var::Text("[[/GENERAL]]")),
            ("specificOpen", Var::Text("[[SPECIFIC]]")),
            ("specificClose", Var::Text("[[/SPECIFIC]]")),
            ("keywordsOpen", Var::Text("[[KEYWORDS]]")),
            ("keywordsClose", Var::Text("[[/KEYWORDS]]")),
            ("removeOpen", Var::Text("[[REMOVE]]")),
            ("removeClose", Var::Text("[[/REMOVE]]")),
        ],
    )
}
