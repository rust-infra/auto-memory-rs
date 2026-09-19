//! Phase 12: replay the reference Picoschema algorithms.
//!
//! `tools/dump_reference_picoschema.py` runs the reference interpreter over
//! `basic_memory.picoschema` (pure functions over plain dicts) and writes
//! `tests/golden/schema/picoschema.json`. This test replays every case against the
//! Rust port, including the two ordered outputs — `suggested_schema` and
//! `unmatched_observations` — whose key order is observable.

use auto_memory::schema::inference::{NoteData, ObservationData, RelationData};
use auto_memory::schema::{
    diff_schema, infer_schema, parse_picoschema, parse_schema_note, resolve_schema, validate_note,
};
use serde_json::{Map, Value, json};
mod common;
use common::load_golden_json;

fn golden() -> Value {
    load_golden_json("schema/picoschema.json")
}

fn cases<'a>(golden: &'a Value, key: &str) -> &'a Vec<Value> {
    golden[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// Assert two JSON objects carry the same keys in the same order.
///
/// `Value` equality ignores object order, and two of the reference outputs are
/// ordered by frequency or first appearance, so equality alone is not enough.
fn assert_key_order(ours: &Value, expected: &Value, context: &str) {
    let (Some(ours), Some(expected)) = (ours.as_object(), expected.as_object()) else {
        return;
    };
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "{context}: key order differs"
    );
    for (key, value) in ours {
        assert_key_order(value, &expected[key], context);
    }
}

#[test]
fn parser_replays_the_reference_picoschema_fields() {
    let golden = golden();
    for case in cases(&golden, "parse") {
        let fields = parse_picoschema(&object(&case["schema"]));
        let ours = serde_json::to_value(&fields).expect("fields");
        assert_eq!(ours, case["fields"], "parse case {}", case["id"]);
    }
}

#[test]
fn schema_notes_replay_the_reference_definition_and_errors() {
    let golden = golden();
    for case in cases(&golden, "parse_schema_note") {
        let definition = parse_schema_note(&case["frontmatter"])
            .unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
        assert_eq!(
            serde_json::to_value(&definition).expect("definition"),
            case["definition"],
            "schema note {}",
            case["id"]
        );
    }
    for case in cases(&golden, "parse_schema_note_errors") {
        let error = parse_schema_note(&case["frontmatter"])
            .expect_err(&format!("{} should fail", case["id"]));
        assert_eq!(error.message, case["error"], "error case {}", case["id"]);
    }
}

#[test]
fn validation_replays_the_reference_reports() {
    let golden = golden();
    for case in cases(&golden, "validate") {
        let schema = parse_schema_note(&case["schema"]).expect("schema");
        let raw = &case["note"];
        let observations: Vec<ObservationData> = raw["observations"]
            .as_array()
            .expect("observations")
            .iter()
            .map(|item| ObservationData {
                category: item["category"].as_str().unwrap_or_default().to_owned(),
                content: item["content"].as_str().unwrap_or_default().to_owned(),
            })
            .collect();
        let relations: Vec<RelationData> = raw["relations"]
            .as_array()
            .expect("relations")
            .iter()
            .map(|item| RelationData {
                relation_type: item["relation_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                target_name: item["target_name"].as_str().unwrap_or_default().to_owned(),
                target_note_type: item["target_note_type"].as_str().map(str::to_owned),
            })
            .collect();
        let frontmatter = (!case["frontmatter"].is_null()).then_some(&case["frontmatter"]);

        let result = validate_note(
            raw["identifier"].as_str().unwrap_or_default(),
            &schema,
            &observations,
            &relations,
            frontmatter,
        );
        let ours = serde_json::to_value(&result).expect("validation");
        assert_eq!(ours, case["result"], "validate case {}", case["id"]);
        assert_key_order(
            &ours["unmatched_observations"],
            &case["result"]["unmatched_observations"],
            &case["id"].to_string(),
        );
    }
}

#[test]
fn inference_replays_the_reference_suggestions() {
    let golden = golden();
    for case in cases(&golden, "infer") {
        let notes = notes_from(&case["notes"]);
        let result = infer_schema(
            case["id"].as_str().unwrap_or_default(),
            &notes,
            0.95,
            0.25,
            5,
        );
        let ours = serde_json::to_value(&result).expect("inference");
        assert_eq!(ours, case["result"], "infer case {}", case["id"]);
        assert_key_order(
            &ours["suggested_schema"],
            &case["result"]["suggested_schema"],
            &case["id"].to_string(),
        );
    }
}

#[test]
fn diff_replays_the_reference_drift_reports() {
    let golden = golden();
    for case in cases(&golden, "diff") {
        let schema = parse_schema_note(&case["schema"]).expect("schema");
        let notes = notes_from(&case["notes"]);
        let drift = diff_schema(&schema, &notes, 0.25, 0.10);
        assert_eq!(
            serde_json::to_value(&drift).expect("drift"),
            case["result"],
            "diff case {}",
            case["id"]
        );
    }
}

#[test]
fn resolver_replays_the_reference_priority_order() {
    let golden = golden();
    let schema_notes = [json!({
        "title": "Person",
        "type": "schema",
        "entity": "person",
        "schema": {"name": "string"},
    })];
    for case in cases(&golden, "resolve") {
        let search = |query: &str| -> Vec<Value> {
            schema_notes
                .iter()
                .filter(|note| {
                    note["entity"] == json!(query)
                        || note["title"]
                            .as_str()
                            .is_some_and(|title| title.to_lowercase() == query.to_lowercase())
                })
                .cloned()
                .collect()
        };
        let definition = resolve_schema(&case["frontmatter"], &search)
            .unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
        let ours = definition
            .map(|definition| serde_json::to_value(&definition).expect("definition"))
            .unwrap_or(Value::Null);
        assert_eq!(ours, case["definition"], "resolve case {}", case["id"]);
    }
}

fn notes_from(raw: &Value) -> Vec<NoteData> {
    raw.as_array()
        .expect("notes")
        .iter()
        .map(|note| NoteData {
            identifier: note["identifier"].as_str().unwrap_or_default().to_owned(),
            observations: note["observations"]
                .as_array()
                .expect("observations")
                .iter()
                .map(|item| ObservationData {
                    category: item["category"].as_str().unwrap_or_default().to_owned(),
                    content: item["content"].as_str().unwrap_or_default().to_owned(),
                })
                .collect(),
            relations: note["relations"]
                .as_array()
                .expect("relations")
                .iter()
                .map(|item| RelationData {
                    relation_type: item["relation_type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    target_name: item["target_name"].as_str().unwrap_or_default().to_owned(),
                    target_note_type: item["target_note_type"].as_str().map(str::to_owned),
                })
                .collect(),
        })
        .collect()
}
