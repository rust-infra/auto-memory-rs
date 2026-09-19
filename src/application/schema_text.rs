//! Markdown renderers for the schema reports.
//!
//! Ports `basic_memory.mcp.tools.schema`: the three report formatters plus the three
//! guidance blocks the tools return instead of an empty report. These strings are a
//! user-visible contract, so they are reproduced character for character (including
//! the em dashes and the trailing newline inside the guidance blocks).

use crate::pycompat::python_title;
use crate::schema::report::{DriftReport, InferenceReport, ValidationReport};

/// Render a validation report as readable markdown.
pub fn format_validation_report(report: &ValidationReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    let type_label = report.note_type.clone().unwrap_or_else(|| "all".to_owned());
    lines.push(format!("# Schema Validation: {type_label}"));
    lines.push(String::new());
    lines.push(format!(
        "Notes: {} | Valid: {} | Warnings: {} | Errors: {}",
        report.total_notes, report.valid_count, report.warning_count, report.error_count
    ));
    lines.push(String::new());

    if !report.type_summaries.is_empty() {
        lines.push("## By Type".to_owned());
        lines.push(String::new());
        for summary in &report.type_summaries {
            if summary.total_entities == 0 {
                lines.push(format!("- **{}**: no notes", summary.note_type));
            } else {
                lines.push(format!(
                    "- **{}**: {}/{} valid",
                    summary.note_type, summary.valid_count, summary.total_notes
                ));
            }
        }
        lines.push(String::new());
    }

    for result in &report.results {
        let status = if result.passed { "valid" } else { "INVALID" };
        lines.push(format!("- **{}** — {}", result.note_identifier, status));
        for warning in &result.warnings {
            lines.push(format!("  - warning: {warning}"));
        }
        for error in &result.errors {
            lines.push(format!("  - error: {error}"));
        }
    }

    lines.join("\n")
}

/// Render an inference report as readable markdown.
pub fn format_inference_report(report: &InferenceReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("# Schema Inference: {}", report.note_type));
    lines.push(String::new());
    lines.push(format!("Notes analyzed: {}", report.notes_analyzed));
    lines.push(String::new());

    if !report.suggested_schema.is_empty() {
        lines.push("## Suggested Schema".to_owned());
        lines.push(String::new());
        lines.push("```yaml".to_owned());
        lines.push("---".to_owned());
        lines.push(format!("title: {}", python_title(&report.note_type)));
        lines.push("type: schema".to_owned());
        lines.push(format!("entity: {}", report.note_type));
        lines.push("version: 1".to_owned());
        lines.push("schema:".to_owned());
        for (field_name, field_def) in &report.suggested_schema {
            lines.push(format!("  {field_name}: {}", yaml_scalar(field_def)));
        }
        lines.push("---".to_owned());
        lines.push("```".to_owned());
        lines.push(String::new());
    }

    if !report.field_frequencies.is_empty() {
        lines.push("## Field Frequencies".to_owned());
        lines.push(String::new());
        for frequency in &report.field_frequencies {
            let percentage = python_percent(frequency.percentage);
            let required_marker = if report.suggested_required.contains(&frequency.name) {
                "required"
            } else {
                "optional"
            };
            let samples = frequency
                .sample_values
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let sample_str = if samples.is_empty() {
                String::new()
            } else {
                format!(" (e.g. {samples})")
            };
            lines.push(format!(
                "- **{}** ({}) — {} ({}/{}) [{}]{}",
                frequency.name,
                frequency.source,
                percentage,
                frequency.count,
                frequency.total,
                required_marker,
                sample_str
            ));
        }
        lines.push(String::new());
    }

    if !report.excluded.is_empty() {
        lines.push("## Excluded (below threshold)".to_owned());
        lines.push(String::new());
        for name in &report.excluded {
            lines.push(format!("- {name}"));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Render a drift report as readable markdown.
pub fn format_drift_report(report: &DriftReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("# Schema Drift: {}", report.note_type));
    lines.push(String::new());

    let has_drift = !report.new_fields.is_empty()
        || !report.dropped_fields.is_empty()
        || !report.cardinality_changes.is_empty();
    if !has_drift {
        lines.push("No drift detected — schema matches actual usage.".to_owned());
        return lines.join("\n");
    }

    if !report.new_fields.is_empty() {
        lines.push("## New Fields (in notes but not in schema)".to_owned());
        lines.push(String::new());
        for field in &report.new_fields {
            lines.push(format!(
                "- **{}** ({}) — {} ({}/{})",
                field.name,
                field.source,
                python_percent(field.percentage),
                field.count,
                field.total
            ));
        }
        lines.push(String::new());
    }

    if !report.dropped_fields.is_empty() {
        lines.push("## Dropped Fields (in schema but rare in notes)".to_owned());
        lines.push(String::new());
        for field in &report.dropped_fields {
            lines.push(format!(
                "- **{}** ({}) — {} ({}/{})",
                field.name,
                field.source,
                python_percent(field.percentage),
                field.count,
                field.total
            ));
        }
        lines.push(String::new());
    }

    if !report.cardinality_changes.is_empty() {
        lines.push("## Cardinality Changes".to_owned());
        lines.push(String::new());
        for change in &report.cardinality_changes {
            lines.push(format!("- {change}"));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Guidance returned when a project has no notes of the requested type.
pub fn no_notes_guidance(note_type: &str, tool_name: &str) -> String {
    format!(
        "# No Notes Found of Type '{note_type}'\n\n\
         `{tool_name}` found no notes with type '{note_type}' in the project.\n\n\
         ## Next Steps\n\n\
         1. **Create notes of this type** — use `write_note` with \
         `note_type=\"{note_type}\"` to create notes\n\
         2. **Check existing types** — use `search_notes` with `note_types` filter to see \
         what types exist\n\
         3. **Browse content** — use `list_directory` or `recent_activity` to see what's \
         in the project\n"
    )
}

/// Guidance returned when validating every type but no schema notes exist.
pub fn no_schemas_defined_guidance(tool_name: &str) -> String {
    format!(
        "# No Schemas Defined\n\n\
         `{tool_name}` was called without `note_type` or `identifier`, which validates \
         every note type that has a schema — but this project has no schema notes yet, \
         so there is nothing to validate.\n\n\
         ## Next Steps\n\n\
         1. **Infer a schema** — run `schema_infer(\"<note_type>\")` to analyze existing \
         notes and get a suggested schema\n\
         2. **Create a schema note** — write a note with `type: schema` and an `entity` \
         field naming the note type it validates\n\
         3. **Re-run** — call `{tool_name}()` again once a schema exists\n"
    )
}

/// Guidance returned when notes exist but no schema covers them.
pub fn no_schema_guidance(note_type: &str, tool_name: &str) -> String {
    format!(
        "# No Schema Found for '{note_type}'\n\n\
         `{tool_name}` requires a schema note to exist for type '{note_type}'.\n\n\
         ## How to Create a Schema\n\n\
         1. **Infer from existing notes** — run `schema_infer(\"{note_type}\")` to analyze \
         your notes and get a suggested schema\n\
         2. **Create a schema note** — write a markdown file with this frontmatter:\n\n\
         ```yaml\n\
         ---\n\
         title: {title}\n\
         type: schema\n\
         entity: {note_type}\n\
         version: 1\n\
         schema:\n\
         \x20\x20name: string, full name\n\
         \x20\x20role?: string, job title\n\
         settings:\n\
         \x20\x20validation: warn\n\
         ---\n\
         ```\n\n\
         Schema fields use Picoschema notation:\n\
         - `field_name: type, description` — required field\n\
         - `field_name?: type, description` — optional field\n\
         - Supported types: `string`, `number`, `boolean`, `string[]`\n\n\
         3. **Index** — run `basic-memory db reindex --search` or wait for the file \
         watcher to pick up the new schema note\n\
         4. **Re-run** — call `{tool_name}(\"{note_type}\")` again\n",
        title = python_title(note_type)
    )
}

/// Python `f"{percentage:.0%}"`.
pub fn python_percent(percentage: f64) -> String {
    format!("{:.0}%", (percentage * 100.0).round_ties_even())
}

/// Render one suggested-schema value the way Python's `str()` would inside an f-string.
fn yaml_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => crate::pycompat::python_str(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{no_schema_guidance, python_percent};
    use crate::pycompat::python_title;

    #[test]
    fn percent_uses_python_rounding() {
        assert_eq!(python_percent(0.25), "25%");
        assert_eq!(python_percent(0.125), "12%");
        assert_eq!(python_percent(0.375), "38%");
        assert_eq!(python_percent(2.0 / 3.0), "67%");
        assert_eq!(python_percent(0.0), "0%");
    }

    #[test]
    fn title_matches_python() {
        assert_eq!(python_title("person"), "Person");
        assert_eq!(python_title("basic_memory"), "Basic_Memory");
        assert_eq!(python_title("meeting note"), "Meeting Note");
        assert_eq!(python_title("v2note"), "V2Note");
    }

    #[test]
    fn schema_guidance_uses_the_title_cased_type() {
        assert!(no_schema_guidance("work_item", "schema_diff").contains("title: Work_Item\n"));
    }
}
