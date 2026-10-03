use super::check;
use shelfy_core::ai::taxonomy_prompt;
use shelfy_core::tags::{VocabTag, graph::CandidateGroup};
#[test]
fn refine_requests_match_desktop() {
    check(
        "ai/taxonomy-prompts/refine",
        |(group,): (CandidateGroup,)| taxonomy_prompt::refine(&group).unwrap(),
    );
}
#[test]
fn alias_requests_match_desktop() {
    check(
        "ai/taxonomy-prompts/aliases",
        |(batch, vocab): (Vec<VocabTag>, Vec<VocabTag>)| {
            taxonomy_prompt::aliases(&batch, &vocab).unwrap()
        },
    );
}
