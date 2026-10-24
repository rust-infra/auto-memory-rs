//! Valid time: `@effective` / `@valid` / `@occurred` / `@due` / `@mentioned` qualifiers.
//!
//! Upstream 0.24.0 (#1426, SPEC-82) lets an observation say *when* its claim holds, and
//! `search_notes` filter by it with `valid_at`, `valid_overlaps` and `time_kind`. Without
//! it a knowledge base can only answer "what mentions X", not "what did we believe on
//! June 10" — the difference between a document store and a memory.
//!
//! Two rules are load-bearing:
//!
//! - a **malformed** qualifier stays in the observation text, so a typo is visible instead
//!   of silently dropping the claim it was meant to qualify;
//! - a filter that does not parse is **refused**, not ignored, because dropping a time
//!   bound answers a different question than the one asked.
//!
//! The vault here is purpose-built rather than the shared fixture: the valid-time cases
//! need authored dates, and adding them to `tests/fixtures/vault` would move the golden
//! corpus.

mod common;

use std::fs;

use common::{Scratch, Session, frame_of};
use serde_json::{Value, json};

/// A vault whose observations carry explicit valid-time qualifiers.
const VAULT: &[(&str, &str)] = &[
    (
        "timeline.md",
        "---\ntitle: Timeline\ntype: note\n---\n\n# Timeline\n\n\
         - [decision] @effective:2026-06-10 The cache layer will use Redis.\n\
         - [decision] @effective[2026-07-01,2026-08-01) The cache layer will use Memcached.\n\
         - [fact] @occurred:\"June 10, 2026\" The launch slipped a week.\n\
         - [task] @due:2026-07-01 Ship the migration.\n\
         - [note] @mentioned:2026-05-01 Someone raised the cache question.\n\
         - [fact] An undated fact about caching.\n\
         - [fact] @effective:not-a-date A typo stays in the text.\n",
    ),
    (
        "plain.md",
        "---\ntitle: Plain\ntype: note\n---\n\n# Plain\n\n\
         - [fact] Redis is a cache.\n",
    ),
];

/// Build the vault in `dir` and return its path.
fn write_vault(dir: &Scratch) -> std::path::PathBuf {
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault dir");
    for (name, body) in VAULT {
        fs::write(vault.join(name), body).expect("write note");
    }
    vault
}

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

/// The observation contents a search page returned, sorted.
fn observation_texts(page: &Value) -> Vec<String> {
    let mut rows: Vec<String> = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|row| {
            let content = row["content"].as_str()?;
            // The entity row carries the whole body; only observation rows are of
            // interest here.
            (row["type"] == json!("observation")).then(|| content.to_owned())
        })
        .collect();
    rows.sort();
    rows
}

/// Run one search and return its JSON payload.
fn search(tag: &str, arguments: Value) -> Value {
    let dir = Scratch::new(tag);
    let vault = write_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {
                       "query": "cache",
                       "entity_types": ["observation"],
                       "output_format": "json",
                       "page_size": 50,
                       "page": 1,
                       "page_size_x": 0,
                       "x": 0,
                   }}}),
    ];
    // The extra keys above are ignored by the lax parser; drop them for clarity.
    let mut requests = requests;
    requests[1]["params"]["arguments"] = arguments;
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{tag}: {frame}");
    payload(frame).clone()
}

/// The same search, but returning the raw frame so a refusal can be asserted.
fn search_frame(tag: &str, arguments: Value) -> Value {
    let dir = Scratch::new(tag);
    let vault = write_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes", "arguments": arguments}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    frame_of(&output.frames, 2).clone()
}

#[test]
fn a_qualifier_is_stripped_from_the_indexed_text() {
    let page = search(
        "valid-strip",
        json!({"query": "Redis", "entity_types": ["observation"],
               "output_format": "json", "page_size": 50}),
    );
    let found = observation_texts(&page);
    // The text the index stores starts at the claim, not at the qualifier.
    assert!(
        found
            .iter()
            .any(|text| text.contains("The cache layer will use Redis.")),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|text| text.contains("@effective")),
        "the qualifier must not stay in the indexed text: {found:?}"
    );
}

#[test]
fn a_malformed_qualifier_stays_in_the_text() {
    let page = search(
        "valid-typo",
        json!({"query": "typo", "entity_types": ["observation"],
               "output_format": "json", "page_size": 50}),
    );
    let found = observation_texts(&page);
    // The claim is still readable, typo and all, rather than vanishing.
    assert!(
        found
            .iter()
            .any(|text| text.contains("@effective:not-a-date")),
        "a malformed qualifier must stay visible: {found:?}"
    );
}

#[test]
fn valid_at_selects_the_claim_that_held_on_that_day() {
    let page = search(
        "valid-at",
        json!({"query": "cache", "valid_at": "2026-06-10",
               "output_format": "json", "page_size": 50}),
    );
    let found = observation_texts(&page);
    // The June 10 effective date covers that day…
    assert!(
        found.iter().any(|text| text.contains("will use Redis")),
        "{found:?}"
    );
    // …while the July window does not.
    assert!(
        !found.iter().any(|text| text.contains("will use Memcached")),
        "a July-only window must not cover June 10: {found:?}"
    );
}

#[test]
fn valid_at_is_half_open_at_the_end() {
    // The July window is `[2026-07-01, 2026-08-01)`: its start day is covered…
    let inside = search(
        "valid-at-start",
        json!({"query": "cache", "valid_at": "2026-07-01",
               "output_format": "json", "page_size": 50}),
    );
    assert!(
        observation_texts(&inside)
            .iter()
            .any(|text| text.contains("will use Memcached")),
        "the start of a range is covered"
    );
    // …and its end day is not.
    let outside = search(
        "valid-at-end",
        json!({"query": "cache", "valid_at": "2026-08-01",
               "output_format": "json", "page_size": 50}),
    );
    assert!(
        !observation_texts(&outside)
            .iter()
            .any(|text| text.contains("will use Memcached")),
        "the end of a half-open range is not covered"
    );
}

#[test]
fn valid_overlaps_selects_a_window_that_intersects() {
    let page = search(
        "valid-overlaps",
        json!({"query": "cache", "valid_overlaps": ["2026-07-15", "2026-07-20"],
               "output_format": "json", "page_size": 50}),
    );
    let found = observation_texts(&page);
    assert!(
        found.iter().any(|text| text.contains("will use Memcached")),
        "{found:?}"
    );
    // The instant-only June 10 claim does not intersect a July window.
    assert!(
        !found.iter().any(|text| text.contains("will use Redis")),
        "{found:?}"
    );
}

#[test]
fn time_kind_selects_one_kind_of_claim() {
    let due = search(
        "valid-kind-due",
        json!({"query": "migration", "time_kind": "due",
               "output_format": "json", "page_size": 50}),
    );
    assert!(
        observation_texts(&due)
            .iter()
            .any(|text| text.contains("Ship the migration")),
        "{due}"
    );
    // A due task is not also an `effective` claim.
    let effective = search(
        "valid-kind-effective",
        json!({"query": "migration", "time_kind": "effective",
               "output_format": "json", "page_size": 50}),
    );
    assert!(
        !observation_texts(&effective)
            .iter()
            .any(|text| text.contains("Ship the migration")),
        "{effective}"
    );
}

#[test]
fn an_undated_observation_is_not_returned_by_a_time_filter() {
    // `An undated fact about caching` has no qualifier, so no time filter may claim it.
    for filter in [
        json!({"valid_at": "2026-06-10"}),
        json!({"valid_overlaps": ["2026-01-01", "2027-01-01"]}),
        json!({"time_kind": "valid"}),
    ] {
        let mut arguments = json!({"query": "undated", "output_format": "json", "page_size": 50});
        for (key, value) in filter.as_object().expect("filter") {
            arguments[key] = value.clone();
        }
        let page = search("valid-undated", arguments);
        assert!(
            observation_texts(&page).is_empty(),
            "an undated observation must not match {filter}: {page}"
        );
    }
}

#[test]
fn an_unparsable_valid_at_is_refused() {
    let frame = search_frame(
        "valid-bad-date",
        json!({"query": "cache", "valid_at": "not-a-date", "output_format": "json"}),
    );
    let error = frame["error"]["message"]
        .as_str()
        .expect("a bad valid_at must fail the call");
    assert!(error.contains("valid_at is not a date"), "{error}");
}

#[test]
fn an_unparsable_time_kind_is_refused_and_lists_the_valid_ones() {
    let frame = search_frame(
        "valid-bad-kind",
        json!({"query": "cache", "time_kind": "sometime", "output_format": "json"}),
    );
    let error = frame["error"]["message"]
        .as_str()
        .expect("a bad time_kind must fail the call");
    assert!(error.contains("invalid time_kind"), "{error}");
    for kind in ["effective", "valid", "occurred", "due", "mentioned"] {
        assert!(error.contains(kind), "the error must list {kind}: {error}");
    }
}

#[test]
fn an_inverted_valid_overlaps_range_is_refused() {
    let frame = search_frame(
        "valid-inverted",
        json!({"query": "cache", "valid_overlaps": ["2026-08-01", "2026-07-01"],
               "output_format": "json"}),
    );
    let error = frame["error"]["message"]
        .as_str()
        .expect("an inverted range must fail the call");
    assert!(error.contains("must be before end"), "{error}");
}

#[test]
fn a_search_without_time_filters_still_returns_every_claim() {
    // The filters are opt-in: an ordinary query keeps today's behavior.
    let page = search(
        "valid-unfiltered",
        json!({"query": "cache", "entity_types": ["observation"],
               "output_format": "json", "page_size": 50}),
    );
    let found = observation_texts(&page);
    assert!(
        found.iter().any(|text| text.contains("will use Redis")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|text| text.contains("will use Memcached")),
        "{found:?}"
    );
}
