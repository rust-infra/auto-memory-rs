//! FTS5 query preparation.
//!
//! Ports `SQLiteSearchRepository._prepare_single_term` / `_prepare_search_term`
//! (Basic Memory 0.23.2) for the golden corpus:
//! - simple terms get a prefix wildcard (`rust` → `rust*`);
//! - multi-word queries without special characters become `word* AND word*`;
//! - terms containing spaces/punctuation become `"term"*` phrases;
//! - boolean operators (AND/OR/NOT) are preserved;
//! - title queries use the same rules without prefix wildcards.

const PROBLEMATIC_CHARS: &[char] = &[
    '"', '\'', '(', ')', '[', ']', '{', '}', '+', '!', '@', '#', '$', '%', '^', '&', '=', '|',
    '\\', '~', '`',
];

const QUOTING_CHARS: &[char] = &[' ', '.', ':', ';', ',', '<', '>', '?', '/', '-'];

fn has_any(term: &str, chars: &[char]) -> bool {
    chars.iter().any(|c| term.contains(*c))
}

/// Prepare one term exactly like the reference `_prepare_single_term`.
pub fn prepare_single_term(term: &str, is_prefix: bool) -> String {
    let term = term.trim();
    if term.is_empty() {
        return String::new();
    }
    if term.contains('*')
        && term
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '*' | '_' | '-'))
    {
        return term.to_owned();
    }

    let mut term = term.to_owned();
    if term.contains(' ') {
        let words: Vec<String> = term
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| "?!.,;:".contains(c)).to_owned())
            .filter(|word| !word.is_empty())
            .collect();
        term = words.join(" ");
        if term.is_empty() {
            return String::new();
        }
    }

    let problematic = has_any(&term, PROBLEMATIC_CHARS);
    let special = has_any(&term, QUOTING_CHARS);

    if problematic || special {
        if term.contains(' ') && !problematic {
            let words: Vec<&str> = term.split_whitespace().collect();
            // Upstream 0.24.0 (#1657): quote only the word that carries punctuation, and
            // require every word to match. Quoting the whole query as one phrase only
            // finds the words when they are adjacent, so `IT-644 cacheability` returned
            // nothing from a note that holds both. A Boolean query keeps its own path: an
            // operator would be quoted into a literal here.
            let has_operator = words
                .iter()
                .any(|word| matches!(*word, "AND" | "OR" | "NOT"));
            if !has_operator {
                let prepared: Vec<String> = words
                    .iter()
                    .map(|word| {
                        let token = if has_any(word, QUOTING_CHARS) {
                            // Quoted so FTS5 reads the punctuation as part of the term.
                            format!("\"{}\"", word.replace('"', "\"\""))
                        } else {
                            (*word).to_owned()
                        };
                        if is_prefix {
                            format!("{token}*")
                        } else {
                            token
                        }
                    })
                    .collect();
                return prepared.join(" AND ");
            }
        }
        let escaped = term.replace('"', "\"\"");
        let is_path = term.contains('/') && term.ends_with(".md");
        return if is_prefix && !is_path {
            format!("\"{escaped}\"*")
        } else {
            format!("\"{escaped}\"")
        };
    }

    if is_prefix { format!("{term}*") } else { term }
}

/// Whether a query carries an FTS5 boolean operator.
///
/// Upstream 0.24.0 (#1683): such a query runs as **full text** even without an explicit
/// `search_type`. The vector half of a hybrid search ignores Boolean operators, so a
/// `NOT` used to exclude nothing and an `AND` used to rank as a bag of words — the query
/// looked like it had been honored while it had not.
#[must_use]
pub fn has_boolean_operators(term: &str) -> bool {
    [" AND ", " OR ", " NOT "]
        .iter()
        .any(|op| format!(" {term} ").contains(op))
}

/// Prepare a boolean query, preserving operators and parenthesised groups.
pub fn prepare_boolean_query(term: &str) -> String {
    let mut prepared = Vec::new();
    for token in term.split_whitespace() {
        if matches!(token.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT") {
            prepared.push(token.to_ascii_uppercase());
            continue;
        }
        let cleaned = token.trim_matches(|c: char| matches!(c, '(' | ')'));
        let prefix = token.starts_with('(');
        let suffix = token.ends_with(')');
        let mut term = prepare_single_term(cleaned, true);
        if prefix {
            term = format!("({term}");
        }
        if suffix {
            term.push(')');
        }
        prepared.push(term);
    }
    prepared.join(" ")
}

/// Prepare a user query for FTS5.
pub fn prepare_fts_query(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prepared = if has_boolean_operators(trimmed) {
        prepare_boolean_query(trimmed)
    } else {
        prepare_single_term(trimmed, true)
    };
    (!prepared.is_empty()).then_some(prepared)
}

/// Prepare the `cjk_grams` leg of a query.
///
/// Upstream 0.24.0 (#1294): a query for a script written without spaces is expanded into
/// the same unigrams and bigrams the index stores, joined with `AND` so a multi-character
/// query keeps its precision. `中文笔记` becomes `"中文" AND "文笔" AND "笔记"` — matching
/// the bigrams is what distinguishes a note about that phrase from one that merely
/// contains `中`.
///
/// Returns `None` when the query carries no unsegmented text, in which case the `cjk_grams`
/// column contributes nothing and the ordinary word index answers alone.
#[must_use]
pub fn prepare_ngram_query(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || !crate::search::ngram::contains_unsegmented_script(trimmed) {
        return None;
    }
    // The bigrams are the precise terms; a one-character run has only its unigram.
    let grams = crate::search::ngram::ngrams(trimmed);
    let bigrams: Vec<&String> = grams
        .iter()
        .filter(|gram| gram.chars().count() == 2)
        .collect();
    let terms: Vec<String> = if bigrams.is_empty() {
        grams
            .iter()
            .filter(|gram| gram.chars().count() == 1)
            .map(|gram| format!("\"{gram}\""))
            .collect()
    } else {
        bigrams.iter().map(|gram| format!("\"{gram}\"")).collect()
    };
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// Prepare a title query (no prefix wildcards, matching the reference title search).
pub fn prepare_title_query(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prepared = prepare_single_term(trimmed, false);
    (!prepared.is_empty()).then_some(prepared)
}

pub use crate::search::relaxation::relaxed_query;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_terms_get_prefix_wildcards() {
        assert_eq!(prepare_fts_query("rust").as_deref(), Some("rust*"));
    }

    #[test]
    fn multi_word_queries_become_and_prefix_terms() {
        assert_eq!(
            prepare_fts_query("emoji unicode").as_deref(),
            Some("emoji* AND unicode*")
        );
    }

    #[test]
    fn special_terms_become_prefix_phrases() {
        assert_eq!(
            prepare_fts_query("zzzz-not-present").as_deref(),
            Some("\"zzzz-not-present\"*")
        );
        // The reference escapes existing quotes: `"""source of truth"""*`.
        assert_eq!(
            prepare_fts_query("\"source of truth\"").as_deref(),
            Some("\"\"\"source of truth\"\"\"*")
        );
    }

    #[test]
    fn boolean_queries_preserve_operators() {
        assert_eq!(
            prepare_fts_query("rust AND architecture").as_deref(),
            Some("rust* AND architecture*")
        );
    }

    #[test]
    fn title_queries_have_no_prefix() {
        assert_eq!(prepare_title_query("Alpha").as_deref(), Some("Alpha"));
    }
    #[test]
    fn a_punctuated_word_beside_other_words_quotes_only_itself() {
        // Upstream 0.24.0 (#1657): `IT-644 cacheability` used to become one exact phrase,
        // which only matches when the words are adjacent in that order.
        assert_eq!(
            prepare_single_term("IT-644 cacheability", true),
            "\"IT-644\"* AND cacheability*"
        );
        assert_eq!(
            prepare_single_term("IT-644 cacheability", false),
            "\"IT-644\" AND cacheability"
        );
    }

    #[test]
    fn plain_multi_word_queries_are_unchanged() {
        assert_eq!(
            prepare_single_term("rust search", true),
            "rust* AND search*"
        );
    }
}
