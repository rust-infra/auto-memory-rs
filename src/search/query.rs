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
            let has_special_in_words = words.iter().any(|word| has_any(word, &QUOTING_CHARS[1..]));
            if !has_special_in_words {
                let prepared: Vec<String> = if is_prefix {
                    words.iter().map(|word| format!("{word}*")).collect()
                } else {
                    words.iter().map(|word| (*word).to_owned()).collect()
                };
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

fn has_boolean_operators(term: &str) -> bool {
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
}
