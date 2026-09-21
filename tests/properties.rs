//! Property tests for the parser and the graph traversal.
//!
//! These are dependency-free: a small LCG generates inputs, and each round asserts an
//! invariant the reference guarantees. They complement the golden corpus (which pins specific
//! documents) by covering shapes nobody wrote a fixture for.

use std::fs;

use auto_memory::domain::permalink::generate_permalink;
use auto_memory::indexing::{RebuildOptions, rebuild_vault};
use auto_memory::markdown::parse_document;
use auto_memory::markdown::serialize::render;
use auto_memory::storage::Store;
mod common;
use common::Scratch;

/// Deterministic pseudo-random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() as usize) % items.len()]
    }
}

/// The parser hands back JSON metadata; the writer wants a YAML mapping.
fn json_metadata_to_yaml(
    metadata: &serde_json::Map<String, serde_json::Value>,
) -> serde_yaml_ng::Mapping {
    let text = serde_json::to_string(metadata).expect("metadata json");
    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) {
        Ok(serde_yaml_ng::Value::Mapping(mapping)) => mapping,
        _ => serde_yaml_ng::Mapping::new(),
    }
}

/// A generated note: frontmatter plus observations, relations, and prose.
fn generated_note(rng: &mut Rng, index: usize) -> String {
    let categories = ["fact", "decision", "requirement", "note", "open-question"];
    let relation_types = ["links_to", "depends_on", "relates_to", "implements"];
    let values = [
        "alpha",
        "beta",
        "gamma",
        "文字",
        "with, comma",
        "with: colon",
        "quoted \"value\"",
        "5",
        "true",
    ];

    let mut text = String::from("---\n");
    text.push_str(&format!("title: Note {index}\n"));
    text.push_str(&format!(
        "type: {}\n",
        rng.pick(&["note", "project", "person"])
    ));
    text.push_str(&format!(
        "tags: [{}, {}]\n",
        rng.pick(&values),
        rng.pick(&values)
    ));
    if rng.next() % 2 == 0 {
        text.push_str(&format!("status: {}\n", rng.pick(&values)));
    }
    text.push_str("---\n\n");
    text.push_str(&format!("# Body {index}\n\n"));
    for _ in 0..(rng.next() % 4) {
        text.push_str(&format!("prose {}\n", rng.pick(&values)));
    }
    for _ in 0..(rng.next() % 3) {
        text.push_str(&format!(
            "- [{}] {}\n",
            rng.pick(&categories),
            rng.pick(&values)
        ));
    }
    for _ in 0..(rng.next() % 3) {
        text.push_str(&format!(
            "- {} [[note-{}]]\n",
            rng.pick(&relation_types),
            rng.next() % 8
        ));
    }
    text
}

/// Parsing a rendered document is a fixed point, and rendering is stable.
#[test]
fn parsing_a_rendered_document_is_a_fixed_point() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for index in 0..200 {
        let path = format!("notes/generated-{index}.md");
        let original = generated_note(&mut rng, index);
        let parsed = parse_document(&path, &original).expect("parse");
        if parsed.frontmatter_error {
            // The generator occasionally emits YAML-hostile values; the reference (and this
            // port) falls back to plain markdown for those, so the fixed-point check only
            // applies to documents whose frontmatter parsed.
            assert!(
                !parsed.had_frontmatter,
                "{path}: malformed YAML is not frontmatter"
            );
            continue;
        }
        let rendered = render(
            &json_metadata_to_yaml(&parsed.frontmatter.metadata),
            &parsed.content,
        );
        let reparsed = parse_document(&path, &rendered).expect("reparse");

        assert_eq!(reparsed.frontmatter.title, parsed.frontmatter.title);
        assert_eq!(reparsed.frontmatter.note_type, parsed.frontmatter.note_type);
        assert_eq!(reparsed.frontmatter.tags, parsed.frontmatter.tags);
        assert_eq!(reparsed.content, parsed.content, "{path}: body");
        assert_eq!(
            reparsed.observations, parsed.observations,
            "{path}: observations"
        );
        assert_eq!(reparsed.relations, parsed.relations, "{path}: relations");
        let rendered_again = render(
            &json_metadata_to_yaml(&reparsed.frontmatter.metadata),
            &reparsed.content,
        );
        assert_eq!(rendered_again, rendered, "{path}: render is stable");
    }
}

/// Every generated observation and relation survives the parser with its label intact.
#[test]
fn observations_and_relations_are_never_lost() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for index in 0..200 {
        let path = format!("notes/generated-{index}.md");
        let original = generated_note(&mut rng, index);
        let parsed = parse_document(&path, &original).expect("parse");

        let expected_observations = original
            .lines()
            .filter(|line| line.starts_with("- [") && !line.contains("[["))
            .count();
        assert_eq!(
            parsed.observations.len(),
            expected_observations,
            "{path}: observation count"
        );
        for observation in &parsed.observations {
            assert!(
                observation.category.is_some(),
                "{path}: a bracketed line keeps its category"
            );
            assert!(
                !observation.content.is_empty(),
                "{path}: observation content"
            );
        }
        assert_eq!(
            parsed.relations.len(),
            original.lines().filter(|line| line.contains("[[")).count(),
            "{path}: relation count"
        );
    }
}

/// Permalink generation is idempotent and strips the characters validation rejects.
#[test]
fn permalinks_are_stable_and_lowercase() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let pieces = [
        "Notes",
        "API v2",
        "my_note",
        "Deep/Nested",
        "中文",
        "v2.0.0",
        "with space",
        "release.notes",
    ];
    for _ in 0..200 {
        let path = format!("{}/{}.md", rng.pick(&pieces), rng.pick(&pieces));
        let permalink = generate_permalink(&path);
        assert!(!permalink.contains(' '), "{path} -> {permalink}");
        assert!(!permalink.contains('_'), "{path} -> {permalink}");
        assert!(!permalink.contains('\\'), "{path} -> {permalink}");
        // A known extension is stripped once, so regenerating is a no-op only once the
        // result no longer carries one (the reference behaves the same way).
        if !permalink.ends_with(".md") {
            assert_eq!(
                generate_permalink(&permalink),
                permalink,
                "{path}: generating from a permalink is a no-op"
            );
        }
    }
    // The reference's documented examples.
    assert_eq!(generate_permalink("docs/My Feature.md"), "docs/my-feature");
    assert_eq!(generate_permalink("specs/API (v2).md"), "specs/api-v2");
    assert_eq!(generate_permalink("Version 2.0.0"), "version-2.0.0");
}

/// `find_related` invariants over a generated vault.
#[tokio::test(flavor = "multi_thread")]
async fn graph_traversal_returns_unique_rows_in_depth_order() {
    let dir = Scratch::new("graph");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let mut rng = Rng(0xdead_beef_cafe_1234);
    for index in 0..12 {
        let mut text = format!("---\ntitle: Note {index}\ntype: note\n---\n\n# Note {index}\n\n");
        for _ in 0..(rng.next() % 3) {
            text.push_str(&format!("- links_to [[note-{}]]\n", rng.next() % 12));
        }
        fs::write(vault.join(format!("note-{index}.md")), text).expect("write");
    }

    let mut store = Store::open_in_memory().await.expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .await
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .await
    .expect("rebuild");

    let seeds: Vec<i64> = store
        .entities(project_id)
        .await
        .expect("entities")
        .iter()
        .map(|entity| entity.id)
        .take(3)
        .collect();
    for depth in [1u32, 2, 3] {
        for max_results in [1u32, 5, 50] {
            let rows = store
                .find_related(project_id, &seeds, depth, max_results, None)
                .await
                .expect("traversal");
            assert!(
                rows.len() <= max_results as usize,
                "depth {depth}: {max_results} cap honoured"
            );

            // The reference groups by `type, id, …, root_id`, so one entity reached from
            // two seeds legitimately appears once per seed — but never twice per seed.
            let mut seen: Vec<(String, i64, i64)> = Vec::new();
            for row in &rows {
                let key = (row.item_type.clone(), row.id, row.root_id);
                assert!(
                    !seen.contains(&key),
                    "depth {depth}: {key:?} returned twice"
                );
                seen.push(key);
                assert!(
                    seeds.contains(&row.root_id),
                    "depth {depth}: every row belongs to a seed"
                );
                assert!(
                    row.depth >= 1,
                    "depth {depth}: the seeds themselves are excluded"
                );
            }

            // The reference orders by `depth, type, id`, so depths never decrease.
            let depths: Vec<i64> = rows.iter().map(|row| row.depth).collect();
            let mut sorted = depths.clone();
            sorted.sort_unstable();
            assert_eq!(
                depths, sorted,
                "depth {depth}: rows come back in depth order"
            );
        }
    }
}
