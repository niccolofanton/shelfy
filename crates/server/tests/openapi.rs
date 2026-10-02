//! The OpenAPI document: valid OpenAPI 3.1, internally consistent, and equal to
//! the committed `crates/server/openapi.json` that the TypeScript client is
//! generated from.
//!
//! After an API change, regenerate the committed copy with
//! `UPDATE_OPENAPI=1 cargo test -p shelfy-server --test openapi` and commit it.
//!
//! Validation uses the official schemas of the OpenAPI Initiative
//! (<https://spec.openapis.org/>, Apache-2.0), vendored in
//! `tests/fixtures/oas-3.1/` with their content unchanged (prettier formatted
//! them): `schema-base` checks the whole document and, with the OAS dialect and
//! vocabulary, every Schema Object in it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use shelfy_server::routes;

const SCHEMA: &str = "https://spec.openapis.org/oas/3.1/schema/2025-09-15";
const SCHEMA_BASE: &str = "https://spec.openapis.org/oas/3.1/schema-base/2025-09-15";
const DIALECT: &str = "https://spec.openapis.org/oas/3.1/dialect/2024-11-10";
const META: &str = "https://spec.openapis.org/oas/3.1/meta/2024-11-10";

fn fixture(name: &str) -> Value {
    let text = match name {
        "schema" => include_str!("fixtures/oas-3.1/schema-2025-09-15.json"),
        "schema-base" => include_str!("fixtures/oas-3.1/schema-base-2025-09-15.json"),
        "dialect" => include_str!("fixtures/oas-3.1/dialect-2024-11-10.json"),
        "meta" => include_str!("fixtures/oas-3.1/meta-2024-11-10.json"),
        _ => unreachable!("unknown fixture {name}"),
    };
    serde_json::from_str(text).expect("fixture is JSON")
}

fn generated() -> Value {
    serde_json::from_str(&routes::openapi_json()).expect("the document is JSON")
}

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

#[test]
fn the_document_is_valid_openapi_3_1() {
    let registry = jsonschema::Registry::new()
        .add(SCHEMA, fixture("schema"))
        .and_then(|r| r.add(DIALECT, fixture("dialect")))
        .and_then(|r| r.add(META, fixture("meta")))
        .and_then(jsonschema::RegistryBuilder::prepare)
        .expect("the OAS schemas load");
    let base = fixture("schema-base");
    assert_eq!(base["$id"], SCHEMA_BASE);
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&base)
        .expect("the OAS schema compiles");

    let doc = generated();
    assert_eq!(doc["openapi"], "3.1.0");
    let errors: Vec<String> = validator
        .iter_errors(&doc)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "invalid OpenAPI 3.1:\n{}",
        errors.join("\n")
    );

    // The validator does catch mistakes, in the document and in its schemas.
    let mut broken = doc.clone();
    broken["paths"]["/health"]["get"]["responses"]["200"]
        .as_object_mut()
        .unwrap()
        .remove("description");
    assert!(!validator.is_valid(&broken));
    let mut broken = doc.clone();
    broken["components"]["schemas"]["Health"]["type"] = Value::from(5);
    assert!(!validator.is_valid(&broken), "Schema Objects are validated");
}

#[test]
fn the_document_is_internally_consistent() {
    let doc = generated();

    // Every reference resolves.
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for reference in &refs {
        let pointer = reference
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("external reference {reference}"));
        assert!(doc.pointer(pointer).is_some(), "dangling {reference}");
    }

    // Every operation has a unique operationId, a tag and the problem default.
    let mut ids = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        assert!(
            path == "/health" || path.starts_with("/api/v1/"),
            "{path} is outside /api/v1"
        );
        for (method, operation) in item.as_object().unwrap() {
            let id = operation["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            assert!(ids.insert(id.to_owned()), "duplicate operationId {id}");
            assert!(operation["tags"].as_array().is_some_and(|t| !t.is_empty()));
            assert_eq!(
                operation["responses"]["default"]["$ref"], "#/components/responses/Problem",
                "{method} {path} lacks the problem response"
            );
        }
    }

    // The error contract: problem+json with the stable code enum.
    let problem = &doc["components"]["responses"]["Problem"]["content"]["application/problem+json"];
    assert_eq!(problem["schema"]["$ref"], "#/components/schemas/Problem");
    let codes = doc["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .expect("ErrorCode is a string enum");
    for code in [
        "invalid_cursor",
        "quota_exceeded",
        "provider_key_invalid",
        "capture_blocked",
    ] {
        assert!(
            codes.iter().any(|c| c == code),
            "{code} missing from ErrorCode"
        );
    }
}

#[test]
fn the_committed_document_is_up_to_date() {
    let path = committed_path();
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, routes::openapi_json()).expect("write openapi.json");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing: run `UPDATE_OPENAPI=1 cargo test -p shelfy-server --test openapi`",
            path.display()
        )
    });
    let committed: Value = serde_json::from_str(&committed).expect("openapi.json is JSON");
    // Compared as JSON: only content counts. (The generated layout is already
    // the one the pre-commit hook's prettier produces.)
    assert!(
        committed == generated(),
        "{} is stale: run `UPDATE_OPENAPI=1 cargo test -p shelfy-server --test openapi` and commit it",
        path.display()
    );
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("$ref", Value::String(reference)) => out.push(reference.clone()),
                    _ => collect_refs(child, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}
