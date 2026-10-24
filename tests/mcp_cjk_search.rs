//! CJK and other unsegmented-script search.
//!
//! Upstream 0.24.0 (#1294) indexes character **unigrams and bigrams** alongside the word
//! index for scripts written without spaces. Without it, FTS5's `unicode61` tokenizer
//! files a whole Chinese sentence as one token: the fixture's `这是一个中文笔记，包含
//! English 混合内容。` becomes one term, so a search for `本地索引` matches nothing even
//! though the note contains that exact phrase.
//!
//! The port keeps the n-grams in their own `cjk_grams` column so they add recall without
//! changing the word index's bm25 length normalization, and retries against that column
//! only when the ordinary word search found nothing.
//!
//! The fixture note is `notes/cjk.md`:
//!
//! ```text
//! title: 中文测试文档
//! tags: [中文, 测试]
//! 这是一个中文笔记，包含 English 混合内容。
//! - [decision] 使用 SQLite 作为本地索引
//! ```

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

fn titles(page: &Value) -> Vec<String> {
    page["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|row| row["title"].as_str().map(str::to_owned))
        .collect()
}

/// Run one search and return its JSON payload.
fn search(tag: &str, query: &str) -> Value {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": query, "output_format": "json",
                                        "page_size": 20}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{tag}: {frame}");
    payload(frame).clone()
}

#[test]
fn a_two_character_chinese_word_finds_its_note() {
    let page = search("cjk-two", "中文");
    assert!(
        titles(&page).contains(&"中文测试文档".to_owned()),
        "中文 must find the note that contains it: {page}"
    );
}

#[test]
fn a_four_character_chinese_phrase_finds_its_note() {
    // The whole point of the bigram index: a phrase inside a run of unsegmented text.
    // `使用 SQLite 作为本地索引` contains `本地索引`.
    let page = search("cjk-phrase", "本地索引");
    assert!(
        titles(&page).contains(&"中文测试文档".to_owned()),
        "本地索引 must find the note that contains it: {page}"
    );
}

#[test]
fn a_phrase_from_the_title_finds_its_note() {
    let page = search("cjk-title", "测试文档");
    assert!(
        titles(&page).contains(&"中文测试文档".to_owned()),
        "测试文档 must find the note whose title carries it: {page}"
    );
}

#[test]
fn a_phrase_from_the_body_finds_its_note() {
    let page = search("cjk-body", "这是一个中文笔记");
    assert!(titles(&page).contains(&"中文测试文档".to_owned()), "{page}");
}

#[test]
fn a_japanese_query_finds_a_japanese_note() {
    // The index covers every unsegmented script, not only Chinese. The fixture has no
    // Japanese note, so this pins that the *query* path accepts one and returns a page
    // rather than failing — a script the indexer cannot handle would error here.
    let page = search("cjk-japanese", "テスト");
    assert!(page.get("results").is_some(), "{page}");
}

#[test]
fn latin_text_still_uses_the_word_index() {
    // The n-gram leg is a fallback, not a replacement: an ordinary English query must
    // still be answered by the word index.
    let page = search("cjk-latin", "rust");
    assert!(!titles(&page).is_empty(), "{page}");
}

#[test]
fn a_mixed_query_does_not_regress_latin_matching() {
    // `这是一个中文笔记，包含 English 混合内容` mixes scripts; the word index still
    // answers the Latin part.
    let page = search("cjk-mixed", "English");
    assert!(!titles(&page).is_empty(), "{page}");
}

#[test]
fn a_chinese_query_that_matches_nothing_returns_an_empty_page() {
    // The fallback must not turn "no such note" into "some note": a phrase absent from the
    // vault stays absent.
    let page = search("cjk-miss", "完全不存在的词汇");
    assert!(
        titles(&page).is_empty(),
        "an absent phrase must not match: {page}"
    );
}

#[test]
fn a_chinese_note_is_readable_by_its_permalink() {
    // The n-gram index is a search feature, not a resolver: reading a note still goes
    // through the identifier chain (path, permalink, exact title). The fixture note's
    // permalink is generated from its path, so it is ASCII and unambiguous — which is
    // exactly why a caller that searched by Chinese text reads the note by permalink.
    let dir = Scratch::new("cjk-read");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "read_note",
                          "arguments": {"identifier": "notes/cjk", "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{frame}");
    let note = payload(frame);
    assert_eq!(note["title"], json!("中文测试文档"));
    // The body comes back verbatim, Chinese included.
    let content = note["content"].as_str().expect("content");
    assert!(content.contains("本地索引"), "{content}");
}

#[test]
fn a_search_by_chinese_text_returns_the_permalink_to_read_it_with() {
    // The round trip an agent actually makes: search by Chinese text, then read by the
    // permalink the search returned.
    let page = search("cjk-roundtrip", "本地索引");
    let row = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|row| row["title"] == json!("中文测试文档"))
        .expect("the Chinese note");
    let permalink = row["permalink"].as_str().expect("permalink");
    assert!(!permalink.is_empty());

    let dir = Scratch::new("cjk-roundtrip-read");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "read_note",
                          "arguments": {"identifier": permalink, "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let note = payload(frame_of(&output.frames, 2));
    assert_eq!(note["title"], json!("中文测试文档"), "{note}");
}
