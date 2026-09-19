//! Phase 15: performance smoke benchmarks.
//!
//! These are opt-in because they generate a few hundred notes and would otherwise slow
//! every `cargo test` run:
//!
//! ```bash
//! cargo test --offline --test benchmarks -- --ignored --nocapture
//! ```
//!
//! They measure, rather than assert a machine-independent number: the printed rates are
//! the point. The assertions are deliberately loose sanity floors (a catastrophic
//! regression like accidentally quadratic indexing still fails), and the numbers here
//! have no bearing on the compatibility contract.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use auto_memory::indexing::{IndexOptions, IndexService};
use auto_memory::search::TextSearchOptions;
use auto_memory::storage::Store;
mod common;
use common::Scratch;

/// Notes in the generated vault.
const VAULT_SIZE: usize = 400;
/// Search queries issued per latency run.
const QUERIES: usize = 200;

/// Write a deterministic pseudo-random vault: notes that link to each other, carry
/// observations of several categories, and vary in size.
fn generate_vault(vault: &Path, notes: usize) {
    let topics = ["rust", "sqlite", "obsidian", "markdown", "graph", "unicode"];
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |bound: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % bound.max(1)
    };

    for index in 0..notes {
        let directory = format!("section-{}", index % 8);
        fs::create_dir_all(vault.join(&directory)).expect("dir");
        let target = (index * 7 + 3) % notes;
        let topic = topics[next(topics.len())];
        let mut body = format!(
            "---\ntitle: Note {index}\ntype: note\ntags: [{topic}]\n---\n\n# Note {index}\n\n\
             A generated note about {topic}. It links to [[section-{}/note-{target}]].\n\n",
            target % 8
        );
        for observation in 0..(next(6) + 1) {
            body.push_str(&format!(
                "- [fact] observation {observation} about {topic}\n"
            ));
        }
        body.push_str(&format!(
            "- relates_to [[section-{}/note-{target}]]\n",
            target % 8
        ));
        for _ in 0..next(40) {
            body.push_str("filler ");
        }
        fs::write(
            vault.join(&directory).join(format!("note-{index}.md")),
            body,
        )
        .expect("write note");
    }
}

fn setup(tag: &str) -> (Scratch, PathBuf, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    generate_vault(&vault, VAULT_SIZE);
    let index = dir.join("memory.db");
    let store = Store::open(&index).expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    (dir, vault, store, project_id)
}

fn report(name: &str, documents: usize, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    println!(
        "{name:<28} {documents:>6} docs in {seconds:>8.3}s  ({:>9.1} docs/s)",
        documents as f64 / seconds
    );
}

#[test]
#[ignore = "opt-in benchmark: cargo test --test benchmarks -- --ignored --nocapture"]
fn benchmark_full_and_incremental_indexing() {
    let (_dir, vault, mut store, project_id) = setup("index");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));

    let started = Instant::now();
    let report_full = service.full_rebuild().expect("full rebuild");
    let elapsed = started.elapsed();
    report("full rebuild", report_full.documents_indexed, elapsed);
    assert_eq!(report_full.documents_indexed, VAULT_SIZE);
    assert!(
        elapsed < Duration::from_secs(120),
        "full rebuild of {VAULT_SIZE} notes took {elapsed:?}"
    );

    // A second pass sees every checksum unchanged, which is the incremental fast path.
    let started = Instant::now();
    let report_unchanged = service.reconcile().expect("reconcile");
    let elapsed = started.elapsed();
    assert_eq!(report_unchanged.unchanged, VAULT_SIZE);
    println!(
        "{:<28} {:>6} docs in {:>8.3}s  (checksum-only pass)",
        "reconcile (no changes)",
        report_unchanged.unchanged,
        elapsed.as_secs_f64()
    );

    // Touch 10% of the vault: only those notes may be rewritten.
    for index in (0..VAULT_SIZE).step_by(10) {
        let path = vault
            .join(format!("section-{}", index % 8))
            .join(format!("note-{index}.md"));
        let mut content = fs::read_to_string(&path).expect("read");
        content.push_str("- [fact] touched by the incremental benchmark\n");
        fs::write(&path, content).expect("write");
    }
    let started = Instant::now();
    let report_incremental = service.reconcile().expect("reconcile");
    let elapsed = started.elapsed();
    report(
        "incremental (10% touched)",
        report_incremental.updated,
        elapsed,
    );
    assert_eq!(report_incremental.updated, VAULT_SIZE.div_ceil(10));
    assert_eq!(report_incremental.added, 0);
}

#[test]
#[ignore = "opt-in benchmark: cargo test --test benchmarks -- --ignored --nocapture"]
fn benchmark_text_search_latency() {
    let (_dir, vault, mut store, project_id) = setup("search");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.full_rebuild().expect("full rebuild");
    }

    let queries = [
        "rust",
        "sqlite obsidian",
        "graph traversal",
        "unicode markdown",
        "note 137",
    ];
    let mut samples = Vec::with_capacity(QUERIES);
    let mut hits = 0usize;
    for round in 0..QUERIES {
        let query = queries[round % queries.len()];
        let started = Instant::now();
        let page = store
            .search_text(
                project_id,
                &TextSearchOptions {
                    query: Some(query.to_owned()),
                    ..TextSearchOptions::default()
                },
            )
            .expect("search");
        samples.push(started.elapsed());
        hits += page.results.len();
    }
    samples.sort_unstable();
    let total: Duration = samples.iter().sum();
    println!(
        "{:<28} {QUERIES:>6} queries in {:>7.3}s  (median {:>6.2}ms, p95 {:>6.2}ms, {:>5.1} hits/query)",
        "text search",
        total.as_secs_f64(),
        samples[samples.len() / 2].as_secs_f64() * 1000.0,
        samples[samples.len() * 95 / 100].as_secs_f64() * 1000.0,
        hits as f64 / QUERIES as f64
    );
    assert!(
        samples[samples.len() / 2] < Duration::from_millis(500),
        "median search latency regressed: {:?}",
        samples[samples.len() / 2]
    );
}

#[test]
#[ignore = "opt-in benchmark: cargo test --test benchmarks -- --ignored --nocapture"]
fn benchmark_semantic_chunking() {
    let (_dir, vault, mut store, project_id) = setup("chunk");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.full_rebuild().expect("full rebuild");
    }
    let rows = store.semantic_rows(project_id).expect("semantic rows");
    let started = Instant::now();
    let mut chunks = 0usize;
    for _ in 0..10 {
        chunks = auto_memory::search::build_chunk_records(&rows).len();
    }
    let elapsed = started.elapsed() / 10;
    println!(
        "{:<28} {chunks:>6} chunks from {} rows in {:>7.3}s  ({:>9.1} rows/s)",
        "chunking (per pass)",
        rows.len(),
        elapsed.as_secs_f64(),
        rows.len() as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE)
    );
    assert!(
        chunks > VAULT_SIZE,
        "every note contributes at least one chunk"
    );
}
