//! Picoschema parser.
//!
//! Ports `basic_memory.picoschema.parser`. A schema is a YAML mapping whose *keys*
//! carry modifiers (`role?`, `tags?(array)`, `status?(enum)`, `meta?(object, note)`)
//! and whose *values* carry the type and an optional description
//! (`Organization, where they work`). Both places may contain commas, parentheses,
//! and brackets, so the key splitter scans from the right for the parenthesis that
//! actually closes the final modifier.

use serde::Serialize;
use serde_json::{Map, Value};
use strum::EnumString;

use crate::pycompat::python_repr;

/// Built-in scalar types; anything else that starts uppercase is an entity reference.
const SCALAR_TYPES: [&str; 5] = ["string", "integer", "number", "boolean", "any"];
/// Modifiers that may appear in the `(...)` suffix of a field key.
const MODIFIER_TYPES: [&str; 3] = ["array", "enum", "object"];

/// How validation findings are recorded for a schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ValidationMode {
    /// Findings become warnings; the note still passes.
    Warn,
    /// Findings become errors; the note fails.
    ///
    /// Compatibility: early schema guidance used `error` for enforcing validation.
    #[strum(serialize = "strict", serialize = "error")]
    Strict,
}

/// One field of a Picoschema definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaField {
    /// Field name without modifiers.
    pub name: String,
    /// Declared type, or `enum`/`object` for the two modifier forms.
    #[serde(rename = "type")]
    pub field_type: String,
    /// `false` when the key ended with `?`.
    pub required: bool,
    /// `(array)` modifier.
    pub is_array: bool,
    /// `(enum)` modifier.
    pub is_enum: bool,
    /// Allowed values for an enum field.
    pub enum_values: Vec<String>,
    /// Description from either the key modifier or the value.
    pub description: Option<String>,
    /// Whether the type names an entity (capitalized, not a scalar).
    pub is_entity_ref: bool,
    /// Nested fields for `(object)`.
    pub children: Vec<SchemaField>,
}

/// A parsed schema note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaDefinition {
    /// Entity type this schema describes.
    pub entity: String,
    /// Schema version (whatever the frontmatter declared).
    pub version: Value,
    /// Content fields.
    pub fields: Vec<SchemaField>,
    /// Validation mode.
    pub validation_mode: ValidationMode,
    /// Fields declared under `settings.frontmatter`.
    pub frontmatter_fields: Vec<SchemaField>,
}

/// Errors the parser raises, carrying the reference's exact `ValueError` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaParseError {
    /// Reference-compatible message.
    pub message: String,
}

impl std::fmt::Display for SchemaParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SchemaParseError {}

/// Parse `settings.validation` into its canonical closed vocabulary.
pub fn parse_validation_mode(value: &Value) -> Result<ValidationMode, SchemaParseError> {
    if let Value::String(text) = value
        && let Ok(mode) = text.parse::<ValidationMode>()
    {
        return Ok(mode);
    }
    Err(SchemaParseError {
        message: format!(
            "Invalid settings.validation value {}; expected one of: 'warn', 'strict', or 'error' (alias for 'strict')",
            match value {
                // Python `repr` quotes strings and leaves other literals bare.
                Value::String(text) => python_repr(text),
                other => crate::pycompat::python_str(other),
            }
        ),
    })
}

/// Parse a Picoschema mapping into fields, preserving mapping order.
pub fn parse_picoschema(schema: &Map<String, Value>) -> Vec<SchemaField> {
    let mut fields = Vec::with_capacity(schema.len());
    for (key, value) in schema {
        let parts = parse_field_key_parts(key);
        let name = parts.name;
        let required = parts.required;

        if parts.is_enum {
            let mut description = parts.description;
            let enum_values = match value {
                Value::Array(items) => items.iter().map(crate::pycompat::python_str).collect(),
                other => {
                    let (values, value_description) =
                        parse_enum_string(&crate::pycompat::python_str(other));
                    if description.is_none() {
                        description = value_description;
                    }
                    values
                }
            };
            fields.push(SchemaField {
                name,
                field_type: "enum".to_owned(),
                required,
                is_array: false,
                is_enum: true,
                enum_values,
                description,
                is_entity_ref: false,
                children: Vec::new(),
            });
            continue;
        }

        if parts.is_object || matches!(value, Value::Object(_)) {
            let children = match value {
                Value::Object(nested) => parse_picoschema(nested),
                _ => Vec::new(),
            };
            fields.push(SchemaField {
                name,
                field_type: "object".to_owned(),
                required,
                is_array: false,
                is_enum: false,
                enum_values: Vec::new(),
                description: parts.description,
                is_entity_ref: false,
                children,
            });
            continue;
        }

        let (type_str, value_description) =
            parse_type_and_description(&crate::pycompat::python_str(value));
        let description = parts.description.or(value_description);
        let is_entity_ref = is_entity_ref_type(&type_str);
        fields.push(SchemaField {
            name,
            field_type: type_str,
            required,
            is_array: parts.is_array,
            is_enum: false,
            enum_values: Vec::new(),
            description,
            is_entity_ref,
            children: Vec::new(),
        });
    }
    fields
}

/// Parse a whole schema note's frontmatter.
pub fn parse_schema_note(frontmatter: &Value) -> Result<SchemaDefinition, SchemaParseError> {
    let empty = Map::new();
    let mapping = frontmatter.as_object().unwrap_or(&empty);

    let entity = mapping.get("entity").filter(|value| is_truthy(value));
    let Some(entity) = entity else {
        return Err(SchemaParseError {
            message: "Schema note missing required 'entity' field in frontmatter".to_owned(),
        });
    };
    let entity = crate::pycompat::python_str(entity);

    let schema_dict = mapping.get("schema").filter(|value| is_truthy(value));
    let Some(Value::Object(schema_dict)) = schema_dict else {
        return Err(SchemaParseError {
            message: "Schema note missing required 'schema' dict in frontmatter".to_owned(),
        });
    };

    let version = mapping.get("version").cloned().unwrap_or(Value::from(1));
    let settings = mapping.get("settings").and_then(Value::as_object);
    let validation_value = settings
        .and_then(|settings| settings.get("validation"))
        .cloned()
        .unwrap_or_else(|| Value::String("warn".to_owned()));
    let validation_mode = parse_validation_mode(&validation_value)?;

    let fields = parse_picoschema(schema_dict);
    let frontmatter_fields = settings
        .and_then(|settings| settings.get("frontmatter"))
        .and_then(Value::as_object)
        .map_or_else(Vec::new, parse_picoschema);

    Ok(SchemaDefinition {
        entity,
        version,
        fields,
        validation_mode,
        frontmatter_fields,
    })
}

/// Components of a field key, mirroring `_parse_field_key_parts`.
struct FieldKeyParts {
    name: String,
    required: bool,
    is_array: bool,
    is_enum: bool,
    is_object: bool,
    description: Option<String>,
}

fn parse_field_key_parts(key: &str) -> FieldKeyParts {
    let (key, modifier, description) = split_modifier_suffix(key);
    let is_array = modifier.as_deref() == Some("array");
    let is_enum = modifier.as_deref() == Some("enum");
    let is_object = modifier.as_deref() == Some("object");
    let (required, name) = match key.strip_suffix('?') {
        Some(stripped) => (false, stripped),
        None => (true, key.as_str()),
    };
    FieldKeyParts {
        name: name.trim().to_owned(),
        required,
        is_array,
        is_enum,
        is_object,
        description,
    }
}

/// Split a trailing `(...)` modifier, scanning right to left for its opening paren.
fn split_modifier_suffix(key: &str) -> (String, Option<String>, Option<String>) {
    let stripped = key.trim_end();
    if !stripped.ends_with(')') {
        return (key.to_owned(), None, None);
    }

    let characters: Vec<char> = stripped.chars().collect();
    let mut open = None;
    let mut depth = 0_i32;
    for index in (0..characters.len()).rev() {
        match characters[index] {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(open) = open else {
        return (key.to_owned(), None, None);
    };

    let modifier_text: String = characters[open + 1..characters.len() - 1].iter().collect();
    let modifier_text = modifier_text.trim();
    let (modifier, separator, description) = match modifier_text.split_once(',') {
        Some((head, tail)) => (head, true, tail),
        None => (modifier_text, false, ""),
    };
    let modifier = modifier.trim();
    if !MODIFIER_TYPES.contains(&modifier) {
        return (key.to_owned(), None, None);
    }

    let key_without_modifier: String = characters[..open].iter().collect();
    let description = if separator {
        Some(description.trim().to_owned())
    } else {
        None
    };
    (
        key_without_modifier.trim_end().to_owned(),
        Some(modifier.to_owned()),
        description.filter(|value| !value.is_empty()),
    )
}

/// Split `type, description`, keeping only the first comma.
fn parse_type_and_description(value: &str) -> (String, Option<String>) {
    match value.split_once(',') {
        Some((type_str, description)) => (
            type_str.trim().to_owned(),
            Some(description.trim().to_owned()),
        ),
        None => (value.trim().to_owned(), None),
    }
}

/// Uppercase first letter and not a scalar type means "entity reference".
fn is_entity_ref_type(type_str: &str) -> bool {
    if SCALAR_TYPES.contains(&type_str) {
        return false;
    }
    type_str.chars().next().is_some_and(char::is_uppercase)
}

/// Extract enum values from a quoted string form, e.g. `[a, b], description`.
fn parse_enum_string(value: &str) -> (Vec<String>, Option<String>) {
    if let Some(rest) = value.strip_prefix('[')
        && let Some((items, tail)) = rest.split_once(']')
    {
        let values = items
            .split(',')
            .map(|item| item.trim().to_owned())
            .collect();
        let description = tail
            .trim_start()
            .strip_prefix(',')
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        return (values, description);
    }
    (vec![value.trim().to_owned()], None)
}

/// Python truthiness for the two guarded lookups (`not entity`, `not schema_dict`).
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_none_or(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}
