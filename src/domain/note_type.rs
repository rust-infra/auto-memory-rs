//! Note-type canonicalization.
//!
//! Mirrors `basic_memory.schemas.base.to_snake_case` / `normalize_note_type`, the
//! canonicalizer the reference applies at every boundary that compares note types
//! (search filters, schema coverage, schema lookups). Stored frontmatter `type`
//! values are *not* rewritten during indexing, so legacy or hand-written spellings
//! like `Person`, `person `, or `note-type` still exist in the database; comparing
//! through this function is what makes them one logical type.

/// Convert a note type to its canonical `snake_case` identity.
///
/// Spaces, hyphens, backslashes, and periods become underscores, camelCase
/// boundaries gain an underscore, and the result is lowercased:
///
/// ```
/// # use basic_mem::domain::note_type::normalize_note_type;
/// assert_eq!(normalize_note_type("BasicMemory"), "basic_memory");
/// assert_eq!(normalize_note_type("Memory Service"), "memory_service");
/// assert_eq!(normalize_note_type("memory-service"), "memory_service");
/// ```
pub fn normalize_note_type(note_type: &str) -> String {
    let stripped = note_type.trim();

    // `re.sub(r"[\s\-\\.]", "_", name)`
    let separated: String = stripped
        .chars()
        .map(|ch| {
            if ch.is_whitespace() || ch == '-' || ch == '\\' || ch == '.' {
                '_'
            } else {
                ch
            }
        })
        .collect();

    // `re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", s1)`
    let mut inserted = String::with_capacity(separated.len());
    let mut previous: Option<char> = None;
    for ch in separated.chars() {
        if let Some(previous_char) = previous
            && (previous_char.is_ascii_lowercase() || previous_char.is_ascii_digit())
            && ch.is_ascii_uppercase()
        {
            inserted.push('_');
        }
        inserted.push(ch);
        previous = Some(ch);
    }

    inserted.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::normalize_note_type;

    #[test]
    fn matches_reference_examples() {
        assert_eq!(normalize_note_type("BasicMemory"), "basic_memory");
        assert_eq!(normalize_note_type("Memory Service"), "memory_service");
        assert_eq!(normalize_note_type("memory-service"), "memory_service");
        assert_eq!(normalize_note_type("Memory_Service"), "memory_service");
        assert_eq!(normalize_note_type("  Person  "), "person");
        assert_eq!(normalize_note_type("person"), "person");
        assert_eq!(normalize_note_type("v2Note"), "v2_note");
        assert_eq!(normalize_note_type("Meeting.Note"), "meeting_note");
        assert_eq!(normalize_note_type(""), "");
    }
}
