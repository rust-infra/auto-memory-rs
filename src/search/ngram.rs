//! Character n-grams for scripts written without spaces.
//!
//! Chinese, Japanese, Korean, Thai, Lao, Tibetan, Myanmar and Khmer do not delimit words
//! with spaces, so `unicode61` indexes a whole sentence as one token: `这是一个中文笔记`
//! is a single term and `中文` never matches it. Upstream 0.24.0 (#1294) fixed that by
//! indexing character **unigrams and bigrams** beside the ordinary word index.
//!
//! The port follows the same shape:
//!
//! - every run of unsegmented-script characters contributes its unigrams and bigrams;
//! - a run shorter than two characters contributes only its unigram, because a bigram of
//!   one character would be the same token;
//! - mixed text keeps its Latin words untouched, so `这是一个 English 笔记` indexes both
//!   `english` as a word and the CJK run as n-grams.
//!
//! Bigrams are what make the index usable: a unigram-only index matches any note
//! containing `中`, which is nearly every Chinese note. The bigram `中文` narrows it, and
//! a query for a longer phrase still works because the query side is expanded the same
//! way (see [`crate::search::query`]).

/// The scripts normally written without spaces between words.
///
/// The ranges are the same ones [`crate::search::relaxation`] uses for its word guard, so
/// the two features agree on what "unsegmented" means.
#[must_use]
pub fn is_unsegmented_script(character: char) -> bool {
    matches!(
        character as u32,
        0x0e00..=0x0e7f   // Thai
        | 0x0e80..=0x0eff // Lao
        | 0x0f00..=0x0fff // Tibetan
        | 0x1000..=0x109f // Myanmar
        | 0x1100..=0x11ff // Hangul Jamo
        | 0x1780..=0x17ff // Khmer
        | 0x19e0..=0x19ff // Khmer symbols
        | 0x3040..=0x30ff // Hiragana + Katakana
        | 0x3130..=0x318f // Hangul compatibility Jamo
        | 0x31f0..=0x31ff // Katakana phonetic extensions
        | 0x3400..=0x4dbf // CJK extension A
        | 0x4e00..=0x9fff // CJK unified ideographs
        | 0xa960..=0xa97f // Hangul Jamo extended A
        | 0xac00..=0xd7af // Hangul syllables
        | 0xd7b0..=0xd7ff // Hangul Jamo extended B
        | 0xf900..=0xfaff // CJK compatibility ideographs
        | 0xff65..=0xff9f // Halfwidth Katakana
    )
}

/// Whether `text` contains at least one character from an unsegmented script.
#[must_use]
pub fn contains_unsegmented_script(text: &str) -> bool {
    text.chars().any(is_unsegmented_script)
}

/// Every unigram and bigram in `text`'s unsegmented runs, in first-appearance order.
///
/// Runs are split on any character that is *not* part of an unsegmented script, so Latin
/// words, digits, punctuation and whitespace all act as boundaries. Duplicates are
/// dropped: the index is a bag of terms, and repeating one only inflates the row.
#[must_use]
pub fn ngrams(text: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut run: Vec<char> = Vec::new();
    let flush = |run: &mut Vec<char>,
                 out: &mut Vec<String>,
                 seen: &mut std::collections::BTreeSet<String>| {
        if run.is_empty() {
            return;
        }
        for window in 1..=2 {
            if run.len() < window {
                continue;
            }
            for start in 0..=(run.len() - window) {
                let gram: String = run[start..start + window].iter().collect();
                if seen.insert(gram.clone()) {
                    out.push(gram);
                }
            }
        }
        run.clear();
    };
    for character in text.chars() {
        if is_unsegmented_script(character) {
            run.push(character);
        } else {
            flush(&mut run, &mut out, &mut seen);
        }
    }
    flush(&mut run, &mut out, &mut seen);
    out
}

/// Append `text`'s n-grams to `parts`, preserving the ordinary words as well.
///
/// Called for every text field that reaches `content_stems`, so a note's title, body and
/// tags all gain n-grams.
pub fn extend_with_ngrams(parts: &mut Vec<String>, text: &str) {
    if !contains_unsegmented_script(text) {
        return;
    }
    parts.extend(ngrams(text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chinese_run_yields_unigrams_and_bigrams() {
        let grams = ngrams("中文笔记");
        // Four unigrams, three bigrams, all distinct.
        assert_eq!(grams.len(), 7, "{grams:?}");
        assert!(grams.contains(&"中".to_owned()));
        assert!(grams.contains(&"中文".to_owned()));
        assert!(grams.contains(&"文笔".to_owned()));
        assert!(grams.contains(&"笔记".to_owned()));
        assert!(
            !grams.contains(&"中笔".to_owned()),
            "no skip-grams: {grams:?}"
        );
    }

    #[test]
    fn a_single_character_run_has_only_its_unigram() {
        assert_eq!(ngrams("中"), vec!["中".to_owned()]);
    }

    #[test]
    fn latin_text_produces_no_ngrams() {
        assert!(ngrams("plain english text").is_empty());
        assert!(!contains_unsegmented_script("plain english"));
    }

    #[test]
    fn mixed_text_splits_the_runs() {
        let grams = ngrams("这是 English 笔记");
        // The Latin word is a boundary, so no bigram spans it.
        assert!(grams.contains(&"这是".to_owned()), "{grams:?}");
        assert!(grams.contains(&"笔记".to_owned()), "{grams:?}");
        assert!(
            !grams.iter().any(|gram| gram.contains("English")),
            "{grams:?}"
        );
        assert!(
            !grams.contains(&"是 ".to_owned()) && !grams.contains(&" 笔".to_owned()),
            "whitespace must not join a run: {grams:?}"
        );
    }

    #[test]
    fn japanese_and_korean_are_unsegmented_too() {
        assert!(contains_unsegmented_script("これはテスト"));
        assert!(contains_unsegmented_script("한국어"));
        assert!(ngrams("テスト").contains(&"テス".to_owned()));
    }

    #[test]
    fn thai_lao_khmer_and_myanmar_are_unsegmented() {
        for sample in ["ภาษาไทย", "ພາສາລາວ", "ភាសាខ្មែរ", "မြန်မာ"]
        {
            assert!(contains_unsegmented_script(sample), "{sample}");
            assert!(!ngrams(sample).is_empty(), "{sample}");
        }
    }

    #[test]
    fn duplicates_are_dropped() {
        // `哈哈` twice: 哈, 哈哈, 哈 — the repeated characters collapse.
        let grams = ngrams("哈哈哈哈");
        assert_eq!(grams.iter().filter(|gram| *gram == "哈").count(), 1);
        assert_eq!(grams.iter().filter(|gram| *gram == "哈哈").count(), 1);
    }

    #[test]
    fn extend_adds_ngrams_only_when_a_run_exists() {
        let mut parts = vec!["title".to_owned()];
        extend_with_ngrams(&mut parts, "plain english");
        assert_eq!(parts, vec!["title".to_owned()]);

        let mut parts = vec!["title".to_owned()];
        extend_with_ngrams(&mut parts, "中文");
        assert!(parts.contains(&"中文".to_owned()), "{parts:?}");
    }

    #[test]
    fn a_cjk_sentence_indexes_every_bigram_of_it() {
        let grams = ngrams("这是一个中文笔记");
        // 8 characters: 8 unigrams + 7 bigrams.
        assert_eq!(grams.len(), 15, "{grams:?}");
        // The query side relies on this: `中文` is a term in the index.
        assert!(grams.contains(&"中文".to_owned()));
    }
}
