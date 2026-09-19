//! Serialized schema reports.
//!
//! Mirrors `basic_memory.schemas.schema`. The reference builds these Pydantic models
//! in `api/v2/routers/schema_router.py` and serializes them with
//! `model_dump(mode="json", exclude_none=True)`, so every `None` is dropped from the
//! JSON surface while empty lists stay. Field order matches the models' declaration
//! order.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::schema::diff::SchemaDrift;
use crate::schema::inference::{FieldFrequency, InferenceResult};
use crate::schema::validator::ValidationResult;

/// JSON dump of a report, matching `model_dump(mode="json", exclude_none=True)`.
pub fn to_json<T: Serialize>(report: &T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(report)
}

// --- Validation ---

/// Result of validating a single schema field against a note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldResultResponse {
    /// Schema field name.
    pub field_name: String,
    /// Declared field type.
    pub field_type: String,
    /// Whether the schema declares the field required.
    pub required: bool,
    /// `present`, `missing`, or `enum_mismatch`.
    pub status: String,
    /// Matched values from the note.
    pub values: Vec<String>,
    /// Explanation, omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Validation result for one note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteValidationResponse {
    /// Reporting identifier.
    pub note_identifier: String,
    /// Entity type of the schema that was applied.
    pub schema_entity: String,
    /// True when no errors were recorded (warnings still pass).
    pub passed: bool,
    /// One entry per schema field.
    pub field_results: Vec<FieldResultResponse>,
    /// Observation categories not covered by the schema, with counts.
    pub unmatched_observations: Map<String, Value>,
    /// Relation types not covered by the schema.
    pub unmatched_relations: Vec<String>,
    /// Findings recorded in `warn` mode.
    pub warnings: Vec<String>,
    /// Findings recorded in `strict` mode.
    pub errors: Vec<String>,
}

/// Per-type rollup used when validating every schema-covered type at once.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeValidationSummary {
    /// Note type covered by the rollup.
    pub note_type: String,
    /// Notes that were actually validated.
    pub total_notes: usize,
    /// Notes that exist for this type.
    pub total_entities: usize,
    /// Notes that passed.
    pub valid_count: usize,
    /// Total warnings across the type.
    pub warning_count: usize,
    /// Total errors across the type.
    pub error_count: usize,
}

/// Full validation report for one or more notes.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct ValidationReport {
    /// Requested (or resolved) note type, omitted in identifier mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_type: Option<String>,
    /// Notes that were validated.
    pub total_notes: usize,
    /// Notes that exist for the target scope.
    pub total_entities: usize,
    /// Notes that passed.
    pub valid_count: usize,
    /// Total warnings.
    pub warning_count: usize,
    /// Total errors.
    pub error_count: usize,
    /// Per-note results.
    pub results: Vec<NoteValidationResponse>,
    /// Per-type breakdown, populated in all-types mode.
    pub type_summaries: Vec<TypeValidationSummary>,
}

impl NoteValidationResponse {
    /// Shape one core validation result as its API response model.
    pub fn from_result(result: &ValidationResult) -> Self {
        Self {
            note_identifier: result.note_identifier.clone(),
            schema_entity: result.schema_entity.clone(),
            passed: result.passed,
            field_results: result
                .field_results
                .iter()
                .map(|field_result| FieldResultResponse {
                    field_name: field_result.field.name.clone(),
                    field_type: field_result.field.field_type.clone(),
                    required: field_result.field.required,
                    status: field_result.status.clone(),
                    values: field_result.values.clone(),
                    message: field_result.message.clone(),
                })
                .collect(),
            unmatched_observations: result.unmatched_observations.clone(),
            unmatched_relations: result.unmatched_relations.clone(),
            warnings: result.warnings.clone(),
            errors: result.errors.clone(),
        }
    }
}

impl ValidationReport {
    /// Assemble a report from already-shaped results.
    pub fn new(
        note_type: Option<String>,
        total_entities: usize,
        results: Vec<NoteValidationResponse>,
    ) -> Self {
        let valid_count = results.iter().filter(|result| result.passed).count();
        let warning_count = results.iter().map(|result| result.warnings.len()).sum();
        let error_count = results.iter().map(|result| result.errors.len()).sum();
        Self {
            note_type,
            total_notes: results.len(),
            total_entities,
            valid_count,
            warning_count,
            error_count,
            results,
            type_summaries: Vec::new(),
        }
    }
}

impl TypeValidationSummary {
    /// Roll one type's results up, mirroring the router's aggregation.
    pub fn new(
        note_type: impl Into<String>,
        total_entities: usize,
        results: &[NoteValidationResponse],
    ) -> Self {
        Self {
            note_type: note_type.into(),
            total_notes: results.len(),
            total_entities,
            valid_count: results.iter().filter(|result| result.passed).count(),
            warning_count: results.iter().map(|result| result.warnings.len()).sum(),
            error_count: results.iter().map(|result| result.errors.len()).sum(),
        }
    }
}

// --- Inference ---

/// Frequency analysis for a single field across notes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldFrequencyResponse {
    /// Field name.
    pub name: String,
    /// `observation` or `relation`.
    pub source: String,
    /// Notes containing the field.
    pub count: usize,
    /// Notes analyzed.
    pub total: usize,
    /// `count / total`.
    pub percentage: f64,
    /// Distinct sample values.
    pub sample_values: Vec<String>,
    /// True when the field usually appears more than once per note.
    pub is_array: bool,
    /// Most common target note type (relations only); omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
}

impl From<&FieldFrequency> for FieldFrequencyResponse {
    fn from(frequency: &FieldFrequency) -> Self {
        Self {
            name: frequency.name.clone(),
            source: frequency.source.clone(),
            count: frequency.count,
            total: frequency.total,
            percentage: frequency.percentage,
            sample_values: frequency.sample_values.clone(),
            is_array: frequency.is_array,
            target_type: frequency.target_type.clone(),
        }
    }
}

/// Inference result with the suggested schema definition.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InferenceReport {
    /// Analyzed note type (canonical form).
    pub note_type: String,
    /// Notes analyzed.
    pub notes_analyzed: usize,
    /// Per-field frequencies.
    pub field_frequencies: Vec<FieldFrequencyResponse>,
    /// Ready-to-use Picoschema mapping.
    pub suggested_schema: Map<String, Value>,
    /// Fields at or above the required threshold.
    pub suggested_required: Vec<String>,
    /// Fields above the optional threshold.
    pub suggested_optional: Vec<String>,
    /// Fields below the optional threshold.
    pub excluded: Vec<String>,
}

impl From<&InferenceResult> for InferenceReport {
    fn from(result: &InferenceResult) -> Self {
        Self {
            note_type: result.note_type.clone(),
            notes_analyzed: result.notes_analyzed,
            field_frequencies: result
                .field_frequencies
                .iter()
                .map(FieldFrequencyResponse::from)
                .collect(),
            suggested_schema: result.suggested_schema.clone(),
            suggested_required: result.suggested_required.clone(),
            suggested_optional: result.suggested_optional.clone(),
            excluded: result.excluded.clone(),
        }
    }
}

// --- Drift ---

/// A field involved in schema drift.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftFieldResponse {
    /// Field name.
    pub name: String,
    /// `observation` or `relation`.
    pub source: String,
    /// Notes containing the field.
    pub count: usize,
    /// Notes analyzed.
    pub total: usize,
    /// `count / total`.
    pub percentage: f64,
}

impl From<&FieldFrequency> for DriftFieldResponse {
    fn from(frequency: &FieldFrequency) -> Self {
        Self {
            name: frequency.name.clone(),
            source: frequency.source.clone(),
            count: frequency.count,
            total: frequency.total,
            percentage: frequency.percentage,
        }
    }
}

/// Schema drift analysis comparing a definition against actual usage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftReport {
    /// Requested note type (as supplied by the caller, not canonicalized).
    pub note_type: String,
    /// Whether a schema was found for this type.
    pub schema_found: bool,
    /// Fields common in notes but not declared.
    pub new_fields: Vec<DriftFieldResponse>,
    /// Fields declared but rare in notes.
    pub dropped_fields: Vec<DriftFieldResponse>,
    /// Human-readable cardinality mismatches.
    pub cardinality_changes: Vec<String>,
}

impl DriftReport {
    /// Report for a type with no schema defined.
    pub fn missing_schema(note_type: impl Into<String>) -> Self {
        Self {
            note_type: note_type.into(),
            schema_found: false,
            new_fields: Vec::new(),
            dropped_fields: Vec::new(),
            cardinality_changes: Vec::new(),
        }
    }
}

impl DriftReport {
    /// Shape a core drift result as its API response model.
    pub fn from_drift(note_type: impl Into<String>, drift: &SchemaDrift) -> Self {
        Self {
            note_type: note_type.into(),
            schema_found: true,
            new_fields: drift
                .new_fields
                .iter()
                .map(DriftFieldResponse::from)
                .collect(),
            dropped_fields: drift
                .dropped_fields
                .iter()
                .map(DriftFieldResponse::from)
                .collect(),
            cardinality_changes: drift.cardinality_changes.clone(),
        }
    }
}
