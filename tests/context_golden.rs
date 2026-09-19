//! Phase 9 compatibility: `build_context` against the reference goldens.
//!
//! The comparison is structural: the whole canonicalized response (primary item,
//! observations, related order, metadata counts, pagination) must equal the
//! reference artifact. Timestamps, uuids, and numeric ids are canonicalized out of
//! the goldens and are therefore not contract values.

mod common;

use std::fs;
use std::path::Path;

use basic_mem::application::context::{
    ContextOptions, MAX_RELATED_RESULTS, build_context, render_markdown, render_plain,
};
use basic_mem::domain::timeframe;
use basic_mem::graph::resolve_entity_path;
use basic_mem::indexing::{RebuildOptions, rebuild_vault};
use basic_mem::storage::Store;
use common::{
    Scratch, canonicalize_text, canonicalize_timestamps, canonicalize_uuids, copy_dir,
    fixtures_vault, repo_root,
};
use serde_json::{Map, Value};

/// Numeric id fields the oracle replaces with `<field>` placeholders.
const ID_KEYS: [&str; 8] = [
    "id",
    "entity_id",
    "observation_id",
    "relation_id",
    "from_entity_id",
    "to_entity_id",
    "from_id",
    "to_id",
];

/// Copy the fixture vault into a throwaway directory.
///
/// The oracle indexes a *copy*, so file creation times are "now"; frontmatter
/// timestamps are what the timeframe filter actually exercises. Reproducing the
/// copy keeps this test independent of when the repository was checked out.
fn temp_vault() -> Scratch {
    let target = Scratch::new("context-vault");
    copy_dir(&fixtures_vault(), target.path());
    target
}

fn store_for(vault: &Path) -> (Store, i64) {
    let mut store = Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        vault,
        &RebuildOptions::new("oracle"),
    )
    .expect("rebuild");
    (store, project_id)
}

/// The oracle CLI defaults to the `7d` timeframe for `tool build-context`.
fn options(depth: u32) -> ContextOptions {
    ContextOptions {
        depth,
        since: Some(timeframe::parse_timeframe("7d").expect("since")),
        ..ContextOptions::default()
    }
}

fn golden_json(name: &str) -> Value {
    let path = repo_root()
        .join("tests/golden/context")
        .join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(&path).expect("golden")).expect("json")
}

fn golden_text(name: &str) -> String {
    let path = repo_root()
        .join("tests/golden/context")
        .join(format!("{name}.txt"));
    canonicalize_text(&fs::read_to_string(&path).expect("golden"))
}

/// Apply the oracle's canonicalization rules to our own response.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let canonical = if ID_KEYS.contains(&key.as_str()) && value.is_i64() {
                        Value::String(format!("<{key}>"))
                    } else {
                        canonicalize(value)
                    };
                    (key.clone(), canonical)
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::String(text) => Value::String(canonicalize_timestamps(&canonicalize_uuids(text))),
        other => other.clone(),
    }
}

/// Stable identity of one related item, used to compare sets instead of orders.
fn related_key(item: &Value) -> String {
    [
        item["type"].as_str().unwrap_or_default(),
        item["title"].as_str().unwrap_or_default(),
        item["permalink"].as_str().unwrap_or_default(),
        item["file_path"].as_str().unwrap_or_default(),
        item["relation_type"].as_str().unwrap_or_default(),
        item["to_name"].as_str().unwrap_or_default(),
        item["to_entity"].as_str().unwrap_or_default(),
    ]
    .join("|")
}

#[test]
fn context_matches_reference_for_golden_cases() {
    let vault = temp_vault();
    let (store, project_id) = store_for(vault.path());

    // The reference assigns entity/relation ids while indexing files concurrently,
    // so its traversal order — and which rows a `max_related` cut keeps — varies
    // between oracle runs. `find_related_replays_the_reference_traversal` pins the
    // ordering rule against the reference ids; here every response field must match
    // once the related set is compared as a set, and truncated cases must at least
    // contain every row the reference kept.
    for (name, url, depth) in [
        ("relations-depth1", "memory://notes/relations", 1),
        ("relations-depth2", "memory://notes/relations", 2),
        ("alpha-depth1", "memory://projects/alpha", 1),
        ("frontmatter-depth1", "memory://notes/simple", 1),
        (
            "duplicates-dup-b",
            "memory://duplicates/dup-b/same-title",
            1,
        ),
        ("cjk-depth1", "memory://notes/cjk", 1),
    ] {
        let context = build_context(&store, project_id, url, &options(depth)).expect("context");
        let actual = canonicalize(&serde_json::to_value(&context).expect("serialize"));
        let expected = golden_json(name);
        assert_eq!(actual["page"], expected["page"], "{name}: page");
        assert_eq!(
            actual["page_size"], expected["page_size"],
            "{name}: page_size"
        );
        assert_eq!(actual["has_more"], expected["has_more"], "{name}: has_more");
        assert_eq!(
            actual["results"][0]["primary_result"], expected["results"][0]["primary_result"],
            "{name}: primary result"
        );
        assert_eq!(
            actual["results"][0]["observations"], expected["results"][0]["observations"],
            "{name}: observations"
        );

        let truncated = context.results[0].related_results.len() as u32
            == ContextOptions::default().max_related;
        let actual_metadata = actual["metadata"].as_object().expect("metadata");
        let expected_metadata = expected["metadata"].as_object().expect("metadata");
        for key in [
            "uri",
            "types",
            "depth",
            "timeframe",
            "generated_at",
            "primary_count",
            "related_count",
            "total_results",
        ] {
            assert_eq!(
                actual_metadata.get(key),
                expected_metadata.get(key),
                "{name}: metadata.{key}"
            );
        }
        // These two derive from the rows the traversal kept, so they can only be
        // compared directly when the `max_related` cut did not apply; the reference
        // keeps a different tail whenever its id assignment differs.
        if !truncated {
            for key in ["total_relations", "total_observations"] {
                assert_eq!(
                    actual_metadata.get(key),
                    expected_metadata.get(key),
                    "{name}: metadata.{key}"
                );
            }
        }

        if truncated {
            // Compare against the untruncated traversal: the reference's rows must all
            // be reachable, even though the cut itself is id-order dependent.
            let unbounded = build_context(
                &store,
                project_id,
                url,
                &ContextOptions {
                    max_related: MAX_RELATED_RESULTS,
                    ..options(depth)
                },
            )
            .expect("unbounded context");
            let available: Vec<String> = unbounded.results[0]
                .related_results
                .iter()
                .map(related_key)
                .collect();
            for item in expected["results"][0]["related_results"]
                .as_array()
                .expect("related")
            {
                assert!(
                    available.contains(&related_key(item)),
                    "{name}: reference kept a row the traversal cannot reach: {item}"
                );
            }
            // The counts must still describe our own rows exactly.
            let relations = context.results[0]
                .related_results
                .iter()
                .filter(|item| item["type"] == "relation")
                .count();
            assert_eq!(
                context.metadata.total_relations, relations,
                "{name}: relations"
            );
            let mut observations = context.results[0].observations.len();
            for item in &context.results[0].related_results {
                let Some(permalink) = item["permalink"].as_str() else {
                    continue;
                };
                if item["type"] != "entity" {
                    continue;
                }
                let entity = resolve_entity_path(&store, project_id, permalink)
                    .expect("resolve")
                    .expect("related entity");
                observations += store
                    .observations_for_entity(entity.id)
                    .expect("observations")
                    .len();
            }
            assert_eq!(
                context.metadata.total_observations, observations,
                "{name}: observations across the kept rows"
            );
        } else {
            let mut actual_related = actual["results"][0]["related_results"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut expected_related = expected["results"][0]["related_results"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            actual_related.sort_by_key(related_key);
            expected_related.sort_by_key(related_key);
            assert_eq!(actual_related, expected_related, "{name}: related results");
        }
    }
}

#[test]
fn find_related_replays_the_reference_traversal() {
    // `graph-rows.json` carries the reference id assignment and
    // `find-related.json` the rows its traversal returns for the same graph, so
    // this test checks the ported SQL row-for-row (including order and the
    // `max_related` truncation) without depending on our own id order.
    let graph: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/index/graph-rows.json"))
            .expect("graph rows"),
    )
    .expect("json");
    let related: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/context/find-related.json"))
            .expect("find-related golden"),
    )
    .expect("json");

    let project_id = graph["entities"][0]["project_id"]
        .as_i64()
        .expect("project");
    let graph_dir = Scratch::new("graph");
    let db_path = graph_dir.join("memory.db");
    let store = Store::open(&db_path).expect("store");

    {
        // Explicit ids: the store's own writer would assign its own sequence.
        let connection = rusqlite::Connection::open(&db_path).expect("connection");
        connection
            .execute(
                "INSERT INTO project (id, external_id, name, permalink, path)
                 VALUES (?1, 'oracle-external', 'oracle', 'oracle', '/tmp/vault')",
                [project_id],
            )
            .expect("project row");
        for entity in graph["entities"].as_array().expect("entities") {
            connection
                .execute(
                    "INSERT INTO entity
                        (id, external_id, project_id, title, note_type, entity_metadata,
                         content_type, permalink, file_path, checksum, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, '{}', 'text/markdown', ?6, ?7, NULL, ?8, ?9)",
                    rusqlite::params![
                        entity["id"].as_i64().expect("id"),
                        entity["external_id"].as_str().expect("external id"),
                        entity["project_id"].as_i64().expect("project"),
                        entity["title"].as_str().expect("title"),
                        entity["note_type"].as_str().expect("note type"),
                        entity["permalink"].as_str(),
                        entity["file_path"].as_str().expect("file path"),
                        entity["created_at"].as_str().expect("created"),
                        entity["updated_at"].as_str().expect("updated"),
                    ],
                )
                .expect("entity row");
        }
        for relation in graph["relations"].as_array().expect("relations") {
            connection
                .execute(
                    "INSERT INTO relation
                        (id, project_id, from_id, to_id, to_name, relation_type, context)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    rusqlite::params![
                        relation["id"].as_i64().expect("id"),
                        relation["project_id"].as_i64().expect("project"),
                        relation["from_id"].as_i64().expect("from"),
                        relation["to_id"].as_i64(),
                        relation["to_name"].as_str().expect("to name"),
                        relation["relation_type"].as_str().expect("type"),
                    ],
                )
                .expect("relation row");
        }
    }

    let since = timeframe::since_bound(&timeframe::parse_timeframe("7d").expect("since"));
    for case in related["cases"].as_array().expect("cases") {
        let seed = case["seed_id"].as_i64().expect("seed");
        let depth = case["depth"].as_u64().expect("depth") as u32;
        let max_related = case["max_related"].as_u64().expect("max related") as u32;
        let rows = store
            .find_related(project_id, &[seed], depth, max_related, Some(&since))
            .expect("traversal");
        let expected = case["rows"].as_array().expect("rows");
        assert_eq!(
            rows.len(),
            expected.len(),
            "{}: related row count",
            case["id"]
        );
        for (row, golden) in rows.iter().zip(expected) {
            assert_eq!(row.item_type, golden["type"].as_str().unwrap(), "type");
            assert_eq!(Some(row.id), golden["id"].as_i64(), "id");
            assert_eq!(Some(row.title.as_str()), golden["title"].as_str(), "title");
            assert_eq!(
                Some(row.permalink.as_str()),
                golden["permalink"].as_str(),
                "permalink"
            );
            assert_eq!(
                Some(row.file_path.as_str()),
                golden["file_path"].as_str(),
                "file_path"
            );
            assert_eq!(row.from_id, golden["from_id"].as_i64(), "from_id");
            assert_eq!(row.to_id, golden["to_id"].as_i64(), "to_id");
            assert_eq!(
                row.relation_type.as_deref(),
                golden["relation_type"].as_str(),
                "relation_type"
            );
            assert_eq!(
                row.to_name.as_deref(),
                golden["to_name"].as_str(),
                "to_name"
            );
            assert_eq!(Some(row.depth), golden["depth"].as_i64(), "depth");
            assert_eq!(Some(row.root_id), golden["root_id"].as_i64(), "root_id");
        }
    }

    drop(store);
    let _ = fs::remove_file(&db_path);
}

#[test]
fn plain_text_matches_reference_golden() {
    // The reference CLI renders the JSON payload client-side, so feeding our
    // renderer the same payload must reproduce the captured outline byte for byte.
    for (name, payload) in [
        ("simple-depth1", "frontmatter-depth1"),
        ("relations-depth2", "relations-depth2"),
    ] {
        let rendered = canonicalize_text(&render_plain(&golden_json(payload)));
        assert_eq!(rendered, golden_text(name), "{name}: plain output differs");
    }
}

#[test]
fn markdown_text_matches_reference_golden() {
    // Captured by replaying the reference `_format_context_markdown` over the raw
    // `build_context` payload (`tools/dump_reference_context_text.py`), because the
    // CLI always requests JSON and renders its own plain outline.
    for name in [
        "relations-depth1",
        "relations-depth2",
        "simple-depth1",
        "alpha-depth1",
    ] {
        let payload = if name == "simple-depth1" {
            "frontmatter-depth1"
        } else {
            name
        };
        let path = repo_root()
            .join("tests/golden/context")
            .join(format!("{payload}.md"));
        let expected = canonicalize_text(&fs::read_to_string(&path).expect("markdown golden"));
        let rendered = canonicalize_text(&render_markdown(&golden_json(payload), "oracle"));
        assert_eq!(rendered, expected, "{name}: markdown output differs");
    }
}

#[test]
fn depth_expands_the_related_set() {
    let vault = temp_vault();
    let (store, project_id) = store_for(vault.path());
    let shallow = build_context(&store, project_id, "memory://notes/relations", &options(1))
        .expect("depth 1");
    let deep = build_context(&store, project_id, "memory://notes/relations", &options(2))
        .expect("depth 2");
    assert!(
        deep.metadata.total_relations >= shallow.metadata.total_relations,
        "deeper traversal must not lose relations"
    );
    assert!(
        deep.metadata.total_relations > 0,
        "fixture relations must be discovered"
    );
}

#[test]
fn max_related_caps_the_result_set() {
    let vault = temp_vault();
    let (store, project_id) = store_for(vault.path());
    let context = build_context(
        &store,
        project_id,
        "memory://notes/relations",
        &ContextOptions {
            max_related: 1,
            ..options(2)
        },
    )
    .expect("context");
    assert_eq!(context.results[0].related_results.len(), 1);
    assert_eq!(context.metadata.total_relations, 1, "relations are capped");
    assert_eq!(context.metadata.related_count, 1);
}

#[test]
fn unresolved_memory_urls_return_an_empty_graph() {
    let vault = temp_vault();
    let (store, project_id) = store_for(vault.path());
    let context = build_context(
        &store,
        project_id,
        "memory://notes/does-not-exist",
        &options(1),
    )
    .expect("context");
    assert!(context.results.is_empty());
    assert_eq!(
        context.metadata.uri.as_deref(),
        Some("notes/does-not-exist")
    );
    assert_eq!(context.metadata.primary_count, 0);
    assert_eq!(context.metadata.related_count, 0);
}

#[test]
fn memory_urls_are_validated_and_resolved() {
    let vault = temp_vault();
    let (store, project_id) = store_for(vault.path());
    assert!(basic_mem::graph::normalize_memory_url("notes/relations").is_ok());
    assert!(basic_mem::graph::normalize_memory_url("memory//bad").is_err());
    assert!(basic_mem::graph::normalize_memory_url("bad?query").is_err());

    let entity = resolve_entity_path(&store, project_id, "notes/relations")
        .expect("resolve")
        .expect("entity");
    assert_eq!(entity.title, "Relations Demo");
    assert!(
        resolve_entity_path(&store, project_id, "notes/missing")
            .expect("resolve")
            .is_none()
    );
}
