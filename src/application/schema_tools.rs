//! The `schema_*` tools' branch decisions and their two surfaces.
//!
//! In the reference these decisions live in `mcp/tools/schema.py`, and the CLI
//! (`bm tool schema-*`) reaches them by calling the MCP tool with
//! `output_format="json"`. Both surfaces are therefore decided in one place: a
//! request is classified first (no schemas, no notes, no schema, a real report, or a
//! failure), and only then rendered as JSON or as markdown guidance.

use serde_json::{Value, json};

use crate::application::schema::SchemaService;
use crate::application::schema_text::{
    format_drift_report, format_inference_report, format_validation_report, no_notes_guidance,
    no_schema_guidance, no_schemas_defined_guidance, python_percent,
};
use crate::schema::report::{DriftReport, InferenceReport, ValidationReport, to_json};

/// Tool name used by `schema_validate`'s guidance.
pub const VALIDATE_TOOL: &str = "schema_validate";

/// What a `schema_validate` request turned into.
pub enum ValidationOutcome {
    /// All-types mode in a project with no schema notes at all.
    NoSchemasDefined,
    /// The scope has no notes of the requested type.
    NoNotes {
        /// Type reported back to the caller.
        effective_type: String,
    },
    /// Notes exist but nothing resolved to a schema.
    NoSchema {
        /// Type reported back to the caller.
        effective_type: String,
    },
    /// A real report.
    Report(Box<ValidationReport>),
    /// Schema authoring failed (the reference's HTTP 400 path).
    Failed {
        /// The parser's message.
        message: String,
    },
}

/// What a `schema_infer` request turned into.
pub enum InferenceOutcome {
    /// Notes were analyzed but nothing met the threshold.
    NoPattern {
        /// Requested note type.
        note_type: String,
        /// Threshold the caller passed.
        threshold: f64,
        /// Notes that were analyzed.
        notes_analyzed: usize,
    },
    /// A real report.
    Report(Box<InferenceReport>),
    /// Schema authoring failed.
    Failed {
        /// Requested note type.
        note_type: String,
        /// The parser's message.
        message: String,
    },
}

/// What a `schema_diff` request turned into.
pub enum DriftOutcome {
    /// No schema is defined for the type.
    NoSchema {
        /// Requested note type.
        note_type: String,
    },
    /// A real report.
    Report(Box<DriftReport>),
    /// Schema authoring failed.
    Failed {
        /// Requested note type.
        note_type: String,
        /// The parser's message.
        message: String,
    },
}

/// Classify one `schema_validate` request.
pub async fn validate(
    service: &SchemaService<'_>,
    note_type: Option<&str>,
    identifier: Option<&str>,
) -> ValidationOutcome {
    let report = match service.validate(note_type, identifier).await {
        Ok(report) => report,
        Err(error) => {
            return ValidationOutcome::Failed {
                message: error.to_string(),
            };
        }
    };

    if note_type.is_none() && identifier.is_none() {
        if report.type_summaries.is_empty() {
            return ValidationOutcome::NoSchemasDefined;
        }
        return ValidationOutcome::Report(Box::new(report));
    }

    let effective_type = note_type
        .map(str::to_owned)
        .or_else(|| report.note_type.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    if report.total_entities == 0 {
        return ValidationOutcome::NoNotes { effective_type };
    }
    if report.total_notes == 0 {
        return ValidationOutcome::NoSchema { effective_type };
    }
    ValidationOutcome::Report(Box::new(report))
}

/// Classify one `schema_infer` request.
pub async fn infer(
    service: &SchemaService<'_>,
    note_type: &str,
    threshold: f64,
) -> InferenceOutcome {
    let report = match service.infer(note_type, threshold).await {
        Ok(report) => report,
        Err(error) => {
            return InferenceOutcome::Failed {
                note_type: note_type.to_owned(),
                message: error.to_string(),
            };
        }
    };
    if report.notes_analyzed > 0 && report.suggested_schema.is_empty() {
        return InferenceOutcome::NoPattern {
            note_type: note_type.to_owned(),
            threshold,
            notes_analyzed: report.notes_analyzed,
        };
    }
    InferenceOutcome::Report(Box::new(report))
}

/// Classify one `schema_diff` request.
pub async fn diff(service: &SchemaService<'_>, note_type: &str) -> DriftOutcome {
    match service.diff(note_type).await {
        Ok(report) if report.schema_found => DriftOutcome::Report(Box::new(report)),
        Ok(_) => DriftOutcome::NoSchema {
            note_type: note_type.to_owned(),
        },
        Err(error) => DriftOutcome::Failed {
            note_type: note_type.to_owned(),
            message: error.to_string(),
        },
    }
}

impl ValidationOutcome {
    /// The `output_format="json"` payload.
    pub fn payload(&self) -> Value {
        match self {
            Self::NoSchemasDefined => json!({ "error": "No schemas defined in this project" }),
            Self::NoNotes { effective_type } => {
                json!({ "error": format!("No notes found of type '{effective_type}'") })
            }
            Self::NoSchema { effective_type } => {
                json!({ "error": format!("No schema found for type '{effective_type}'") })
            }
            Self::Report(report) => to_json(report).unwrap_or(Value::Null),
            Self::Failed { message } => {
                json!({ "error": format!("Schema validation failed: {message}") })
            }
        }
    }

    /// The text surface.
    pub fn text(&self) -> String {
        match self {
            Self::NoSchemasDefined => no_schemas_defined_guidance(VALIDATE_TOOL),
            Self::NoNotes { effective_type } => no_notes_guidance(effective_type, VALIDATE_TOOL),
            Self::NoSchema { effective_type } => no_schema_guidance(effective_type, VALIDATE_TOOL),
            Self::Report(report) => format_validation_report(report),
            Self::Failed { message } => format!(
                "# Schema Validation Failed\n\n\
                 Error validating schemas: {message}\n\n\
                 ## Troubleshooting\n\
                 1. Ensure schema notes exist (type: schema) for the target note type\n\
                 2. Check that notes have the correct type in frontmatter\n\
                 3. Verify the project has been indexed: `auto-memory status`\n"
            ),
        }
    }
}

impl InferenceOutcome {
    /// The `output_format="json"` payload.
    pub fn payload(&self) -> Value {
        match self {
            Self::NoPattern {
                note_type,
                threshold,
                ..
            } => json!({
                "error": format!(
                    "No schema pattern found for '{note_type}' (threshold: {})",
                    python_percent(*threshold)
                ),
            }),
            Self::Report(report) => to_json(report).unwrap_or(Value::Null),
            Self::Failed { note_type, message } => {
                let _ = note_type;
                json!({ "error": format!("Schema inference failed: {message}") })
            }
        }
    }

    /// The text surface.
    pub fn text(&self) -> String {
        match self {
            Self::NoPattern {
                note_type,
                threshold,
                notes_analyzed,
            } => format!(
                "# No Schema Pattern Found\n\n\
                 Analyzed {notes_analyzed} notes of type '{note_type}', but no observation or \
                 relation appeared in enough notes to suggest a schema (threshold: {}).\n\n\
                 This usually means '{note_type}' is too broad — the notes don't share a \
                 consistent structure.\n\n\
                 ## Suggestions\n\
                 1. **Use a more specific type** — try `search_notes` with `note_types` \
                 filter to see what types exist\n\
                 2. **Lower the threshold** — \
                 `schema_infer(\"{note_type}\", threshold=0.1)` to include rarer fields\n\
                 3. **Create typed notes** — use `write_note` with a specific `note_type` \
                 (e.g., \"person\", \"meeting\") to build consistent structure\n",
                python_percent(*threshold)
            ),
            Self::Report(report) => format_inference_report(report),
            Self::Failed { note_type, message } => format!(
                "# Schema Inference Failed\n\n\
                 Error inferring schema for type '{note_type}': {message}\n\n\
                 ## Troubleshooting\n\
                 1. Ensure notes of type '{note_type}' exist in the project\n\
                 2. Try searching: `search_notes(\"{note_type}\", \
                 note_types=[\"{note_type}\"])`\n\
                 3. Verify the project has been indexed: `auto-memory status`\n"
            ),
        }
    }
}

impl DriftOutcome {
    /// The `output_format="json"` payload.
    pub fn payload(&self) -> Value {
        match self {
            Self::NoSchema { note_type } => {
                json!({ "error": format!("No schema found for type '{note_type}'") })
            }
            Self::Report(report) => to_json(report).unwrap_or(Value::Null),
            Self::Failed { note_type, message } => {
                let _ = note_type;
                json!({ "error": format!("Schema diff failed: {message}") })
            }
        }
    }

    /// The text surface.
    pub fn text(&self) -> String {
        match self {
            Self::NoSchema { note_type } => no_schema_guidance(note_type, "schema_diff"),
            Self::Report(report) => format_drift_report(report),
            Self::Failed { note_type, message } => format!(
                "# Schema Diff Failed\n\n\
                 Error detecting drift for type '{note_type}': {message}\n\n\
                 ## Troubleshooting\n\
                 1. Ensure a schema note exists for type '{note_type}'\n\
                 2. Ensure notes of type '{note_type}' exist in the project\n\
                 3. Verify the project has been indexed: `auto-memory status`\n"
            ),
        }
    }
}
