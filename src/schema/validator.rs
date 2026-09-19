//! Validate a note's observations and relations against a schema.
//!
//! Ports `basic_memory.picoschema.validator`. The mapping rules are what tie schemas
//! to the existing note format:
//!
//! | schema declaration          | grounded in                        |
//! |-----------------------------|------------------------------------|
//! | `field: string`             | observation `[field] value`        |
//! | `field?(array): string`     | several `[field]` observations     |
//! | `field?: EntityType`        | relation `field [[Target]]`        |
//! | `field?(enum): [values]`    | observation `[field] value` in set |
//!
//! Validation is soft: unmatched observations and relations are reported but never
//! fail the note. Only missing *required* fields and enum mismatches produce
//! diagnostics, and only `strict` mode turns those into errors.

use crate::schema::inference::{ObservationData, RelationData};
use crate::schema::parser::{SchemaDefinition, SchemaField, ValidationMode};
use serde::Serialize;
use serde_json::{Map, Value};

/// Validation result for a single schema field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldResult {
    /// The schema field that was checked.
    pub field: SchemaField,
    /// `present`, `missing`, or `enum_mismatch`.
    pub status: String,
    /// Matched (or offending) values.
    pub values: Vec<String>,
    /// Human-readable explanation.
    pub message: Option<String>,
}

/// Complete validation result for one note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationResult {
    /// Reporting identifier.
    pub note_identifier: String,
    /// Entity type of the schema that was applied.
    pub schema_entity: String,
    /// True when no errors were recorded (warnings are fine).
    pub passed: bool,
    /// One entry per schema field, content fields first.
    pub field_results: Vec<FieldResult>,
    /// Observation categories the schema did not mention, with counts.
    pub unmatched_observations: Map<String, Value>,
    /// Relation types the schema did not mention.
    pub unmatched_relations: Vec<String>,
    /// Findings recorded in `warn` mode.
    pub warnings: Vec<String>,
    /// Findings recorded in `strict` mode.
    pub errors: Vec<String>,
}

/// Validate one note against a schema.
pub fn validate_note(
    note_identifier: &str,
    schema: &SchemaDefinition,
    observations: &[ObservationData],
    relations: &[RelationData],
    frontmatter: Option<&Value>,
) -> ValidationResult {
    let observations_by_category = group_observations(observations);
    let relations_by_type = group_relations(relations);

    let mut result = ValidationResult {
        note_identifier: note_identifier.to_owned(),
        schema_entity: schema.entity.clone(),
        passed: true,
        field_results: Vec::new(),
        unmatched_observations: Map::new(),
        unmatched_relations: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let mut matched_categories: Vec<String> = Vec::new();
    let mut matched_relation_types: Vec<String> = Vec::new();

    for schema_field in &schema.fields {
        let field_result =
            validate_field(schema_field, &observations_by_category, &relations_by_type);
        if schema_field.is_entity_ref {
            matched_relation_types.push(schema_field.name.clone());
        } else {
            matched_categories.push(schema_field.name.clone());
        }

        if field_result.status == "missing" && schema_field.required {
            record_issue(
                &mut result,
                missing_field_message(schema_field),
                schema.validation_mode,
            );
        } else if field_result.status == "enum_mismatch" {
            let message = field_result
                .message
                .clone()
                .unwrap_or_else(|| format!("Field '{}' has invalid enum value", schema_field.name));
            record_issue(&mut result, message, schema.validation_mode);
        }
        result.field_results.push(field_result);
    }

    if let Some(frontmatter) = frontmatter
        && !schema.frontmatter_fields.is_empty()
    {
        for frontmatter_field in &schema.frontmatter_fields {
            let field_result = validate_frontmatter_field(frontmatter_field, frontmatter);
            if field_result.status == "missing" && frontmatter_field.required {
                record_issue(
                    &mut result,
                    format!(
                        "Missing required frontmatter key: {}",
                        frontmatter_field.name
                    ),
                    schema.validation_mode,
                );
            } else if field_result.status == "enum_mismatch" {
                let message = field_result.message.clone().unwrap_or_else(|| {
                    format!(
                        "Frontmatter key '{}' has invalid enum value",
                        frontmatter_field.name
                    )
                });
                record_issue(&mut result, message, schema.validation_mode);
            }
            result.field_results.push(field_result);
        }
    }

    for (category, values) in &observations_by_category {
        if !matched_categories.contains(category) {
            let count = values.as_array().map_or(0, Vec::len);
            result
                .unmatched_observations
                .insert(category.clone(), Value::from(count));
        }
    }
    for relation_type in relations_by_type.keys() {
        if !matched_relation_types.contains(relation_type) {
            result.unmatched_relations.push(relation_type.clone());
        }
    }

    result
}

fn validate_field(
    schema_field: &SchemaField,
    observations_by_category: &Map<String, Value>,
    relations_by_type: &Map<String, Value>,
) -> FieldResult {
    if schema_field.is_entity_ref {
        return validate_entity_ref_field(schema_field, relations_by_type);
    }
    if schema_field.is_enum {
        return validate_enum_field(schema_field, observations_by_category);
    }
    validate_observation_field(schema_field, observations_by_category)
}

fn validate_observation_field(
    schema_field: &SchemaField,
    observations_by_category: &Map<String, Value>,
) -> FieldResult {
    let values = lookup(observations_by_category, &schema_field.name);
    if values.is_empty() {
        return FieldResult {
            field: schema_field.clone(),
            status: "missing".to_owned(),
            values: Vec::new(),
            message: Some(missing_field_message(schema_field)),
        };
    }
    FieldResult {
        field: schema_field.clone(),
        status: "present".to_owned(),
        values,
        message: None,
    }
}

fn validate_entity_ref_field(
    schema_field: &SchemaField,
    relations_by_type: &Map<String, Value>,
) -> FieldResult {
    let targets = lookup(relations_by_type, &schema_field.name);
    if targets.is_empty() {
        return FieldResult {
            field: schema_field.clone(),
            status: "missing".to_owned(),
            values: Vec::new(),
            message: Some(format!(
                "Missing relation: {} (no '{} [[...]]' relation found)",
                schema_field.name, schema_field.name
            )),
        };
    }
    FieldResult {
        field: schema_field.clone(),
        status: "present".to_owned(),
        values: targets,
        message: None,
    }
}

fn validate_enum_field(
    schema_field: &SchemaField,
    observations_by_category: &Map<String, Value>,
) -> FieldResult {
    let values = lookup(observations_by_category, &schema_field.name);
    if values.is_empty() {
        return FieldResult {
            field: schema_field.clone(),
            status: "missing".to_owned(),
            values: Vec::new(),
            message: Some(missing_field_message(schema_field)),
        };
    }
    let invalid: Vec<String> = values
        .iter()
        .filter(|value| !schema_field.enum_values.contains(value))
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return FieldResult {
            field: schema_field.clone(),
            status: "enum_mismatch".to_owned(),
            values,
            message: Some(format!(
                "Field '{}' has invalid value(s): {} (allowed: {})",
                schema_field.name,
                invalid.join(", "),
                schema_field.enum_values.join(", ")
            )),
        };
    }
    FieldResult {
        field: schema_field.clone(),
        status: "present".to_owned(),
        values,
        message: None,
    }
}

fn validate_frontmatter_field(schema_field: &SchemaField, frontmatter: &Value) -> FieldResult {
    let value = frontmatter
        .get(&schema_field.name)
        .filter(|value| !value.is_null());
    let Some(value) = value else {
        return FieldResult {
            field: schema_field.clone(),
            status: "missing".to_owned(),
            values: Vec::new(),
            message: Some(format!("Missing frontmatter key: {}", schema_field.name)),
        };
    };

    if schema_field.is_enum {
        let text = crate::pycompat::python_str(value);
        if !schema_field.enum_values.contains(&text) {
            return FieldResult {
                field: schema_field.clone(),
                status: "enum_mismatch".to_owned(),
                values: vec![text.clone()],
                message: Some(format!(
                    "Frontmatter key '{}' has invalid value: {} (allowed: {})",
                    schema_field.name,
                    text,
                    schema_field.enum_values.join(", ")
                )),
            };
        }
        return FieldResult {
            field: schema_field.clone(),
            status: "present".to_owned(),
            values: vec![text],
            message: None,
        };
    }

    if let Value::Array(items) = value {
        return FieldResult {
            field: schema_field.clone(),
            status: "present".to_owned(),
            values: items.iter().map(crate::pycompat::python_str).collect(),
            message: None,
        };
    }
    FieldResult {
        field: schema_field.clone(),
        status: "present".to_owned(),
        values: vec![crate::pycompat::python_str(value)],
        message: None,
    }
}

fn record_issue(result: &mut ValidationResult, message: String, mode: ValidationMode) {
    match mode {
        ValidationMode::Warn => result.warnings.push(message),
        ValidationMode::Strict => {
            result.errors.push(message);
            result.passed = false;
        }
    }
}

/// Group observation contents by category, keeping first-appearance order.
fn group_observations(observations: &[ObservationData]) -> Map<String, Value> {
    let mut grouped = Map::new();
    for observation in observations {
        push_value(&mut grouped, &observation.category, &observation.content);
    }
    grouped
}

/// Group relation target names by relation type, keeping first-appearance order.
fn group_relations(relations: &[RelationData]) -> Map<String, Value> {
    let mut grouped = Map::new();
    for relation in relations {
        push_value(&mut grouped, &relation.relation_type, &relation.target_name);
    }
    grouped
}

fn push_value(map: &mut Map<String, Value>, key: &str, value: &str) {
    if let Some(Value::Array(items)) = map.get_mut(key) {
        items.push(Value::String(value.to_owned()));
        return;
    }
    map.insert(
        key.to_owned(),
        Value::Array(vec![Value::String(value.to_owned())]),
    );
}

fn lookup(map: &Map<String, Value>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::Array(items)) => items.iter().map(crate::pycompat::python_str).collect(),
        _ => Vec::new(),
    }
}

fn missing_field_message(schema_field: &SchemaField) -> String {
    let kind = if schema_field.required {
        "required"
    } else {
        "optional"
    };
    if schema_field.is_entity_ref {
        return format!(
            "Missing {kind} field: {} (no '{} [[...]]' relation found)",
            schema_field.name, schema_field.name
        );
    }
    format!(
        "Missing {kind} field: {} (expected [{}] observation)",
        schema_field.name, schema_field.name
    )
}
