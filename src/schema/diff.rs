//! Schema drift detection.
//!
//! Ports `basic_memory.picoschema.diff`. It reuses the inference analysis and then
//! compares the inferred frequencies against the declared schema fields, reporting
//! three kinds of drift: fields that are common in notes but undeclared, fields that
//! are declared but rare (or absent), and cardinality mismatches.

use serde::Serialize;

use crate::schema::inference::{FieldFrequency, NoteData, analyze_observations, analyze_relations};
use crate::schema::parser::SchemaDefinition;

/// Result of comparing a schema against actual note usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SchemaDrift {
    /// Fields common in notes but not declared in the schema.
    pub new_fields: Vec<FieldFrequency>,
    /// Fields declared in the schema but rare or absent in notes.
    pub dropped_fields: Vec<FieldFrequency>,
    /// Human-readable cardinality mismatches.
    pub cardinality_changes: Vec<String>,
}

/// Compare a schema against actual note usage.
pub fn diff_schema(
    schema: &SchemaDefinition,
    notes: &[NoteData],
    new_field_threshold: f64,
    dropped_field_threshold: f64,
) -> SchemaDrift {
    let total = notes.len();
    if total == 0 {
        return SchemaDrift::default();
    }

    let mut frequencies = analyze_observations(notes, total, 3);
    frequencies.extend(analyze_relations(notes, total, 3));

    let declared: Vec<String> = schema
        .fields
        .iter()
        .map(|field| field.name.clone())
        .collect();
    let mut result = SchemaDrift::default();

    for frequency in &frequencies {
        if !declared.contains(&frequency.name) && frequency.percentage >= new_field_threshold {
            result.new_fields.push(frequency.clone());
        }
    }

    for schema_field in &schema.fields {
        match frequencies
            .iter()
            .find(|frequency| frequency.name == schema_field.name)
        {
            // The field never appears in any note: report a synthetic zero row.
            None => result.dropped_fields.push(FieldFrequency {
                name: schema_field.name.clone(),
                source: if schema_field.is_entity_ref {
                    "relation".to_owned()
                } else {
                    "observation".to_owned()
                },
                count: 0,
                total,
                percentage: 0.0,
                sample_values: Vec::new(),
                is_array: false,
                target_type: None,
            }),
            Some(frequency) if frequency.percentage < dropped_field_threshold => {
                result.dropped_fields.push(frequency.clone());
            }
            Some(_) => {}
        }
    }

    for schema_field in &schema.fields {
        let Some(frequency) = frequencies
            .iter()
            .find(|frequency| frequency.name == schema_field.name)
        else {
            continue;
        };
        if schema_field.is_array && !frequency.is_array {
            result.cardinality_changes.push(format!(
                "{}: schema declares array but usage is typically single-value",
                schema_field.name
            ));
        } else if !schema_field.is_array && frequency.is_array {
            result.cardinality_changes.push(format!(
                "{}: schema declares single-value but usage is typically array",
                schema_field.name
            ));
        }
    }

    result
}
