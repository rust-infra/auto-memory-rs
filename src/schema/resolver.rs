//! Resolve the applicable schema for a note.
//!
//! Ports `basic_memory.picoschema.resolver`. Resolution is priority-ordered:
//! inline `schema` mapping, then an explicit `schema: <reference>` string, then an
//! implicit match on the note's own `type`, then nothing. Returning nothing is a
//! normal outcome — most notes have no schema.

use serde_json::Value;

use crate::schema::parser::{
    SchemaDefinition, parse_picoschema, parse_schema_note, parse_validation_mode,
};

/// Look up schema notes by query. The reference takes an async callable; callers here
/// supply whatever index lookup they have.
pub type SchemaSearchFn<'a> = &'a dyn Fn(&str) -> Vec<Value>;

/// Resolve the schema for a note's frontmatter.
pub fn resolve_schema(
    note_frontmatter: &Value,
    search: SchemaSearchFn<'_>,
) -> Result<Option<SchemaDefinition>, crate::schema::parser::SchemaParseError> {
    let empty = serde_json::Map::new();
    let frontmatter = note_frontmatter.as_object().unwrap_or(&empty);

    if let Some(Value::Object(schema_dict)) = frontmatter.get("schema") {
        return Ok(Some(schema_from_inline(schema_dict, note_frontmatter)?));
    }

    if let Some(Value::String(reference)) = frontmatter.get("schema") {
        let results = search(reference);
        if let Some(first) = results.first() {
            return parse_schema_note(first).map(Some);
        }
    }

    if let Some(note_type) = frontmatter.get("type").and_then(Value::as_str)
        && !note_type.is_empty()
    {
        let results = search(note_type);
        if let Some(first) = results.first() {
            return parse_schema_note(first).map(Some);
        }
    }

    Ok(None)
}

/// Build a definition from an inline schema mapping, deriving metadata from the note.
pub(crate) fn schema_from_inline(
    schema_dict: &serde_json::Map<String, Value>,
    note_frontmatter: &Value,
) -> Result<SchemaDefinition, crate::schema::parser::SchemaParseError> {
    let fields = parse_picoschema(schema_dict);
    let entity = note_frontmatter
        .get("type")
        .map(crate::pycompat::python_str)
        .unwrap_or_else(|| "unknown".to_owned());
    let validation_value = note_frontmatter
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get("validation"))
        .cloned()
        .unwrap_or_else(|| Value::String("warn".to_owned()));
    Ok(SchemaDefinition {
        entity,
        version: Value::from(1),
        fields,
        validation_mode: parse_validation_mode(&validation_value)?,
        frontmatter_fields: Vec::new(),
    })
}
