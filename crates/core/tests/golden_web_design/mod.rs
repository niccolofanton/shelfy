use super::check;
use serde_json::{Value, json};
use shelfy_core::ai::{
    prompts::{self, Task},
    web_design,
};
#[test]
fn design_v2_contract_matches_the_desktop() {
    check("ai/web-design/contract", |_: Vec<Value>| {
        let schema = prompts::response_schema(Task::WebDesign).unwrap();
        json!({"system":prompts::system_prompt(Task::WebDesign,&[]).unwrap(),"schema":schema.value,"name":schema.name})
    });
}
#[test]
fn every_design_field_and_measured_facet_matches_the_desktop() {
    check(
        "ai/web-design/map",
        |(raw, post, model): (Value, Value, String)| web_design::map(&raw, &post, &model),
    );
}
