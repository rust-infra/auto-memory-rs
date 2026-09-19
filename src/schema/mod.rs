//! Picoschema: parse, resolve, validate, infer, and diff note schemas.
//!
//! Mirrors `basic_memory.picoschema`. Schemas are ordinary notes with
//! `type: schema`, so there is no new data model — a schema field maps either to an
//! observation category or to a relation type, which is what lets validation reuse
//! the same index the rest of the core reads.

pub mod diff;
pub mod inference;
pub mod parser;
pub mod report;
pub mod resolver;
pub mod validator;

pub use diff::{SchemaDrift, diff_schema};
pub use inference::{
    FieldFrequency, InferenceResult, NoteData, ObservationData, RelationData, infer_schema,
};
pub use parser::{
    SchemaDefinition, SchemaField, ValidationMode, parse_picoschema, parse_schema_note,
    parse_validation_mode,
};
pub use report::{
    DriftFieldResponse, DriftReport, FieldFrequencyResponse, FieldResultResponse, InferenceReport,
    NoteValidationResponse, TypeValidationSummary, ValidationReport,
};
pub use resolver::resolve_schema;
pub use validator::{FieldResult, ValidationResult, validate_note};
