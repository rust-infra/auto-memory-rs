//! The zero-result relaxation retry for full-text queries.
//!
//! Ports `basic_memory.repository.search_query` (the eligibility guards and the
//! backend-ready rendering) plus `SQLiteSearchRepository._relaxed_fts_term`.
//!
//! When a strict FTS5 query matches nothing, the reference retries it as an OR of
//! prefixed words. That retry is powerful enough to hurt: `when OR did OR a` matches
//! loud wrong documents that displace genuine results, so relaxation is gated on the
//! *shape* of the query rather than on the query text alone:
//!
//! - quoted and explicit-boolean queries are never second-guessed;
//! - fewer than three word tokens never relaxes (`New Feature` over-broadens);
//! - a numeric token marks an identifier-like query (`SPEC 16`) and never relaxes;
//! - whitespace-separated CJK terms relax from two words up, because they are not
//!   delimited by spaces the way the token guard assumes;
//! - interrogative and function words are pruned before the OR is built.

use unicode_general_category::get_general_category;

/// Interrogative and function words that contribute lexical noise under OR.
const RELAXATION_STOPWORDS: [&str; 47] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "did", "do", "does", "for", "from",
    "had", "has", "have", "how", "i", "in", "is", "it", "of", "on", "or", "our", "that", "the",
    "their", "they", "this", "to", "was", "we", "were", "what", "when", "where", "which", "who",
    "whom", "whose", "why", "will", "with", "you", "your",
];

/// Punctuation trimmed from the edge of a whitespace-delimited relaxed word.
const RELAXATION_EDGE_PUNCTUATION: &str = "?!.,;:，。！？；：、";

/// The one format character that marks a word *boundary* (Thai, Khmer) rather than
/// living inside a word; every other format character is a rendering artifact.
const RELAXATION_WORD_SEPARATOR_FORMAT: char = '\u{200b}';

/// Orthographic joiners stay in place when a term is cleaned: they are written in
/// the text, so the stored note has them too.
const RELAXATION_ORTHOGRAPHIC_JOINERS: [char; 2] = ['\u{200c}', '\u{200d}'];

/// UAX #29 word-internal punctuation, minus colons and full stops (those carry
/// project structure: `tag:value`, file extensions, permalinks).
const RELAXATION_WORD_INTERNAL_PUNCTUATION: [char; 9] = [
    '\'', '\u{2018}', '\u{2019}', '\u{ff07}', '\u{00b7}', '\u{0387}', '\u{055f}', '\u{05f3}',
    '\u{05f4}',
];

/// Scripts normally written without spaces between words.
fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x11ff
            | 0x3040..=0x30ff
            | 0x3130..=0x318f
            | 0x31f0..=0x31ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xa960..=0xa97f
            | 0xac00..=0xd7af
            | 0xd7b0..=0xd7ff
            | 0xf900..=0xfaff
            | 0xff65..=0xff9f
    )
}

/// Unicode general-category abbreviation (`Mn`, `Nd`, `Cf`, …), as Python's
/// `unicodedata.category` reports it.
fn category(character: char) -> &'static str {
    get_general_category(character).abbreviation()
}

/// Whether an invisible format character belongs to the word around it.
fn is_word_internal_format(character: char) -> bool {
    category(character) == "Cf" && character != RELAXATION_WORD_SEPARATOR_FORMAT
}

/// Whether a character hangs off the one before it rather than standing alone.
fn is_attached(character: char) -> bool {
    is_word_internal_format(character) || category(character).starts_with('M')
}

/// Drop format characters left at a token's end, where they separate rather than join.
fn strip_trailing_formats(token: &str) -> &str {
    let mut end = token.len();
    while end > 0 {
        let character = token[..end].chars().next_back().expect("char");
        if !is_word_internal_format(character) {
            break;
        }
        end -= character.len_utf8();
    }
    &token[..end]
}

/// Whether the token so far ends in a letter, looking past what hangs off it.
fn base_before(current: &[char]) -> bool {
    for character in current.iter().rev() {
        if is_attached(*character) {
            continue;
        }
        return character.is_alphabetic();
    }
    false
}

/// Whether a letter follows the punctuation, looking past what hangs off it.
fn base_after(text: &[char], index: usize) -> bool {
    for character in text.iter().skip(index + 1) {
        if is_attached(*character) {
            continue;
        }
        return character.is_alphabetic();
    }
    false
}

/// Whether a non-alphanumeric character belongs to the word being read.
fn is_token_continuation(text: &[char], index: usize, current: &[char]) -> bool {
    let character = text[index];
    if is_attached(character) {
        return true;
    }
    if RELAXATION_WORD_INTERNAL_PUNCTUATION.contains(&character) {
        return base_before(current) && base_after(text, index);
    }
    false
}

/// Split text into the word tokens the relaxation guards count.
///
/// A token is a run of alphanumeric characters together with the combining marks,
/// join controls, and apostrophes written inside it. Counting marks as separators
/// would cut abugidas and decomposed text into fragments, letting one word look like
/// several and clear the three-token guard.
pub fn relaxation_word_tokens(text: &str) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for (index, character) in characters.iter().enumerate() {
        // A leading mark, join control, or apostrophe has no base character to attach
        // to, so it cannot open a token.
        if character.is_alphanumeric()
            || (!current.is_empty() && is_token_continuation(&characters, index, &current))
        {
            current.push(*character);
        } else if !current.is_empty() {
            flush_token(&mut current, &mut tokens);
        }
    }
    flush_token(&mut current, &mut tokens);
    tokens
}

fn flush_token(current: &mut Vec<char>, tokens: &mut Vec<String>) {
    let token: String = current.iter().collect();
    let token = strip_trailing_formats(&token);
    if !token.is_empty() {
        tokens.push(token.to_owned());
    }
    current.clear();
}

/// The token without the characters that only ever attach to another one.
fn token_core(token: &str) -> String {
    token
        .chars()
        .filter(|character| {
            !is_word_internal_format(*character) && !category(*character).starts_with('M')
        })
        .collect()
}

/// Whether a token is a bare number, and so identifier-like rather than a word.
///
/// Classified by Unicode category rather than `is_numeric()`: Han numerals are
/// letters (category `Lo`) and ordinary content words in CJK prose.
fn is_numeric_token(token: &str) -> bool {
    let core = token_core(token);
    !core.is_empty()
        && core
            .chars()
            .all(|character| category(character).starts_with('N'))
}

/// Every form of a word that could match how the note happens to be stored.
///
/// A format character is invisible, so `foo\u{ad}bar` indexes as two tokens while
/// `foobar` indexes as one; both forms are emitted so the OR covers whichever the
/// note actually holds.
fn relaxation_term_variants(word: &str) -> Vec<String> {
    let cleaned: String = word
        .chars()
        .filter(|character| {
            RELAXATION_ORTHOGRAPHIC_JOINERS.contains(character)
                || !is_word_internal_format(*character)
        })
        .collect();
    if cleaned.is_empty() {
        return Vec::new();
    }
    if cleaned == word {
        vec![word.to_owned()]
    } else {
        vec![cleaned, word.to_owned()]
    }
}

/// Expand words into backend-ready terms, then drop duplicates (case-insensitively,
/// keeping first-seen order).
fn emit_relaxation_terms(words: &[String]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for word in words {
        for variant in relaxation_term_variants(word) {
            let key = variant.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            terms.push(variant);
        }
    }
    terms
}

/// Split whitespace-delimited relaxed words, preserving CJK words.
fn split_relaxation_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| RELAXATION_EDGE_PUNCTUATION.contains(character))
        })
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_alnum(word: &str) -> bool {
    !word.is_empty() && word.chars().all(char::is_alphanumeric)
}

/// The content-bearing words an OR-relaxed query would use, or `None` when
/// relaxation must not run.
pub fn relaxed_query_words(search_text: Option<&str>) -> Option<Vec<String>> {
    let text = search_text?;
    let stripped = text.trim();
    if stripped.is_empty() || stripped.contains('"') || has_boolean_operators(stripped) {
        return None;
    }

    // Eligibility runs on raw whitespace-delimited words, before stopword filtering.
    let cjk_words = split_relaxation_words(stripped);
    let has_cjk_term = cjk_words.iter().any(|word| word.chars().any(is_cjk));

    if has_cjk_term {
        // CJK terms are not delimited by spaces the way the token guard assumes, so
        // two or more of them may relax.
        if cjk_words.len() < 2 || cjk_words.iter().any(|word| is_numeric_token(word)) {
            return None;
        }
        let pruned: Vec<String> = cjk_words
            .iter()
            .filter(|word| {
                is_alnum(word) && !RELAXATION_STOPWORDS.contains(&word.to_lowercase().as_str())
            })
            .cloned()
            .collect();
        let relaxed = emit_relaxation_terms(&pruned);
        // Punctuation/stopword pruning can leave only one term; the short-query guard
        // still applies after pruning.
        return (relaxed.len() >= 2).then_some(relaxed);
    }

    let tokens = relaxation_word_tokens(&stripped.to_lowercase());
    if tokens.len() < 3 || tokens.iter().any(|token| is_numeric_token(token)) {
        return None;
    }
    let pruned: Vec<String> = tokens
        .iter()
        .filter(|token| !RELAXATION_STOPWORDS.contains(&token.as_str()))
        .cloned()
        .collect();
    let words = if pruned.is_empty() { tokens } else { pruned };
    let terms = emit_relaxation_terms(&words);
    (!terms.is_empty()).then_some(terms)
}

fn has_boolean_operators(term: &str) -> bool {
    [" AND ", " OR ", " NOT "]
        .iter()
        .any(|operator| format!(" {term} ").contains(operator))
}

/// Render one relaxed word as an FTS5-safe prefix expression.
///
/// A word can contain an apostrophe (`don't`); interpolated bare it is FTS5 syntax,
/// not text, so the whole expression fails to parse and the retry silently returns
/// nothing — the exact silent-empty failure this fallback exists to prevent.
fn relaxed_fts_term(word: &str) -> String {
    if word.contains('\'') || word.contains('"') {
        format!("\"{}\"*", word.replace('"', "\"\""))
    } else {
        format!("{word}*")
    }
}

/// Relaxed OR fallback for a strict query that matched nothing.
pub fn relaxed_query(input: &str) -> Option<String> {
    let words = relaxed_query_words(Some(input))?;
    Some(
        words
            .iter()
            .map(|word| relaxed_fts_term(word))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

#[cfg(test)]
mod tests {
    use super::{relaxation_word_tokens, relaxed_query, relaxed_query_words};

    #[test]
    fn relaxation_requires_three_tokens_without_numbers() {
        assert_eq!(
            relaxed_query("zzzz-not-present").as_deref(),
            Some("zzzz* OR not* OR present*")
        );
        assert!(relaxed_query("two words").is_none());
        assert!(relaxed_query("rust AND arch").is_none());
    }

    /// Stopwords are pruned before the OR is built, which is what keeps a query made
    /// only of noise words from matching half the vault.
    #[test]
    fn stopwords_are_pruned_from_the_relaxed_or() {
        assert_eq!(
            relaxed_query("zzz-nothing-matches-this").as_deref(),
            Some("zzz* OR nothing* OR matches*")
        );
        // Every term was a stopword plus a filler: pruning leaves the original tokens.
        assert_eq!(
            relaxed_query("alpha beta this").as_deref(),
            Some("alpha* OR beta*")
        );
    }

    #[test]
    fn numeric_tokens_disable_relaxation() {
        assert!(relaxed_query("root note 1").is_none());
        assert!(relaxed_query("SPEC 16 report").is_none());
    }

    #[test]
    fn cjk_queries_relax_from_two_words() {
        assert_eq!(
            relaxed_query("中文 笔记").as_deref(),
            Some("中文* OR 笔记*")
        );
        assert!(relaxed_query("中文").is_none());
    }

    #[test]
    fn apostrophes_are_quoted_for_fts() {
        assert_eq!(
            relaxed_query("don't panic now").as_deref(),
            Some("\"don't\"* OR panic* OR now*")
        );
    }

    #[test]
    fn tokens_keep_word_internal_apostrophes_and_marks() {
        assert_eq!(
            relaxation_word_tokens("don't panic now"),
            ["don't", "panic", "now"]
        );
        // A hyphen is a separator, so the token count sees four words here.
        assert_eq!(relaxation_word_tokens("zzz-nothing-matches-this").len(), 4);
        // Marks and joiners stay inside the word they are written in.
        assert_eq!(
            relaxation_word_tokens("e\u{301}clair note"),
            ["e\u{301}clair", "note"]
        );
        assert_eq!(
            relaxation_word_tokens("می\u{200c}رود x y"),
            ["می\u{200c}رود", "x", "y"]
        );
    }

    #[test]
    fn queries_with_few_terms_never_relax() {
        assert!(relaxed_query_words(Some("two words")).is_none());
        assert!(relaxed_query_words(Some("")).is_none());
        assert!(relaxed_query_words(None).is_none());
        assert!(relaxed_query_words(Some("\"quoted phrase\"")).is_none());
    }
}
