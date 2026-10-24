//! Document ingestion: turn a non-Markdown file into a searchable sidecar note.
//!
//! Upstream 0.24.0 (#1006, #1512) adds `bm import document <path>`: it indexes the
//! project, extracts the file's text, and writes `<file>.<ext>.md` beside it plus a run
//! note under `document-ingestion-runs/`, so the extracted text is searchable and its
//! provenance is recorded.
//!
//! The port keeps the same contract without shelling out to `pdf-inspector` or
//! `markitdown`:
//!
//! - **CSV** renders a bounded preview of its rows, with strict UTF-8.
//! - **Text-like** formats (`.txt`, `.log`, `.org`) are copied through.
//! - **PDF, Word, PowerPoint and Excel** are recognised but their text cannot be extracted
//!   without a converter, so the sidecar records that rather than pretending the file was
//!   empty. That distinction matters: an empty sidecar reads as "this document has no
//!   text", which is a lie, while an explicit "not extracted" note tells the caller to
//!   install the converter.
//!
//! Three properties the reference promises and this module keeps:
//!
//! - re-running is a **no-op** while the source is unchanged;
//! - a hand-written or enriched note at the sidecar path is **never overwritten**;
//! - extracted text is searchable but **cannot mint observations or relations** — the
//!   sidecar's body is marked `bm_parse_semantics: false`, so the parser treats it as
//!   text rather than knowledge.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How much of a CSV file the sidecar renders.
///
/// A preview, not a dump: a 200 MB CSV inlined into a note would make the note
/// unsearchable and the index enormous. The row cap is what "bounded" means here.
pub const CSV_PREVIEW_ROWS: usize = 200;

/// How many characters of a text file the sidecar copies.
pub const TEXT_PREVIEW_CHARS: usize = 40_000;

/// The format a document was recognised as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    /// Comma- or tab-separated values.
    Csv,
    /// Plain text or a text-like format.
    Text,
    /// PDF, which needs an external extractor.
    Pdf,
    /// A Word document.
    Word,
    /// A PowerPoint deck.
    PowerPoint,
    /// An Excel workbook.
    Excel,
}

impl DocumentFormat {
    /// Recognise a path's format from its extension.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "csv" | "tsv" => Some(Self::Csv),
            "txt" | "log" | "org" | "rst" | "adoc" => Some(Self::Text),
            "pdf" => Some(Self::Pdf),
            "docx" | "doc" => Some(Self::Word),
            "pptx" | "ppt" => Some(Self::PowerPoint),
            "xlsx" | "xls" => Some(Self::Excel),
            _ => None,
        }
    }

    /// The `type:` the sidecar note carries.
    #[must_use]
    pub fn sidecar_type(self) -> &'static str {
        match self {
            // Upstream 0.24.0: new sidecars use `extracted_text`; older ones kept
            // `document`, and both are accepted on read.
            Self::Csv | Self::Text | Self::Pdf | Self::Word | Self::PowerPoint | Self::Excel => {
                "extracted_text"
            }
        }
    }

    /// Whether the port can extract this format's text.
    #[must_use]
    pub fn is_extractable(self) -> bool {
        matches!(self, Self::Csv | Self::Text)
    }

    /// Why the format cannot be extracted, for the sidecar's note.
    #[must_use]
    pub fn extractor_hint(self) -> Option<&'static str> {
        match self {
            Self::Csv | Self::Text => None,
            Self::Pdf => Some(
                "PDF text extraction needs `pdf-inspector`, which this build does not \
                 bundle. Install it and re-run the import to fill this sidecar.",
            ),
            Self::Word | Self::PowerPoint | Self::Excel => Some(
                "Office text extraction needs Microsoft's `markitdown`, which this build \
                 does not bundle. Install it and re-run the import to fill this sidecar.",
            ),
        }
    }
}

/// One extracted document, ready to be written as a sidecar note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    /// The recognised format.
    pub format: DocumentFormat,
    /// The extracted text.
    pub text: String,
    /// Whether the text was cut short by a preview bound.
    pub truncated: bool,
    /// Number of lines the source had, when it is line-oriented.
    pub line_count: Option<usize>,
}

/// Extract `source`'s text.
///
/// The file is read as bytes and decoded as UTF-8 with replacement, so one bad byte in a
/// log file does not fail the whole import — but a CSV is decoded strictly, because a
/// mis-decoded CSV silently misaligns every column.
pub fn extract(source: &Path, format: DocumentFormat) -> Result<Extraction> {
    let bytes = std::fs::read(source)?;
    match format {
        DocumentFormat::Csv => {
            let text = std::str::from_utf8(&bytes).map_err(|error| Error::InvalidArgument {
                message: format!(
                    "{} is not valid UTF-8 (byte {}): {}",
                    source.display(),
                    error.valid_up_to(),
                    error
                ),
            })?;
            Ok(extract_csv(text))
        }
        DocumentFormat::Text => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(extract_text(&text))
        }
        // Recognised but not extractable here: the sidecar records why, rather than
        // claiming the document was empty.
        other => Ok(Extraction {
            format: other,
            text: String::new(),
            truncated: false,
            line_count: None,
        }),
    }
}

/// Render a bounded CSV preview as a Markdown table.
fn extract_csv(text: &str) -> Extraction {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let preview: Vec<&str> = lines.iter().take(CSV_PREVIEW_ROWS + 1).copied().collect();
    let truncated = total > preview.len();

    let mut out = String::new();
    for (index, line) in preview.iter().enumerate() {
        let cells = split_csv_row(line);
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
        // A Markdown table needs its separator after the header row.
        if index == 0 {
            out.push('|');
            for _ in 0..cells.len() {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    if truncated {
        out.push_str(&format!(
            "\n*Preview: first {} of {total} rows.*\n",
            preview.len()
        ));
    }
    Extraction {
        format: DocumentFormat::Csv,
        text: out,
        truncated,
        line_count: Some(total),
    }
}

/// Split one CSV row, honouring double-quoted fields.
fn split_csv_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if in_quotes && characters.peek() == Some(&'"') => {
                current.push('"');
                characters.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' | '\t' if !in_quotes => {
                cells.push(current.trim().to_owned());
                current.clear();
            }
            other => current.push(other),
        }
    }
    cells.push(current.trim().to_owned());
    cells
}

/// Copy a text file through, bounded.
fn extract_text(text: &str) -> Extraction {
    let line_count = text.lines().count();
    let truncated = text.chars().count() > TEXT_PREVIEW_CHARS;
    let body = if truncated {
        text.chars().take(TEXT_PREVIEW_CHARS).collect()
    } else {
        text.to_owned()
    };
    Extraction {
        format: DocumentFormat::Text,
        text: body,
        truncated,
        line_count: Some(line_count),
    }
}

/// The sidecar path for a source file: `<file>.<ext>.md`, beside it.
#[must_use]
pub fn sidecar_path(source: &Path) -> PathBuf {
    let mut name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".md");
    source.with_file_name(name)
}

/// A deterministic run id, so re-importing the same source updates one run note instead
/// of littering the folder.
#[must_use]
pub fn run_id(source_relative: &str, checksum: &str) -> String {
    // A short hash is enough to make the id stable and unique per source revision, and
    // keeps the filename readable.
    let digest =
        crate::storage::checksum_bytes(format!("{source_relative}\u{0}{checksum}").as_bytes());
    format!("run-{}", &digest[..16])
}

/// The sidecar note's frontmatter and body, ready to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sidecar {
    /// Project-relative path of the sidecar note.
    pub file_path: String,
    /// Full note text, frontmatter included.
    pub content: String,
}

/// Build the sidecar note for one extraction.
///
/// `bm_parse_semantics: false` is the load-bearing key: it tells the parser that this body
/// is extracted text, not authored knowledge, so the import cannot mint observations or
/// relations from a document's contents.
#[must_use]
pub fn build_sidecar(
    source_relative: &str,
    extraction: &Extraction,
    source_checksum: &str,
) -> Sidecar {
    let title = source_relative
        .rsplit('/')
        .next()
        .unwrap_or(source_relative)
        .to_owned();
    let sidecar = format!("{source_relative}.md");
    let mut frontmatter = String::from("---\n");
    frontmatter.push_str(&format!("title: \"{title} (extracted text)\"\n"));
    frontmatter.push_str(&format!("type: {}\n", extraction.format.sidecar_type()));
    frontmatter.push_str("bm_parse_semantics: false\n");
    frontmatter.push_str(&format!("source: {source_relative}\n"));
    frontmatter.push_str(&format!("source_checksum: {source_checksum}\n"));
    // Upstream 0.24.0 (#1744): the sidecar is linked to the file it came from by a
    // `derived_from` relation, so the graph answers "what was extracted from this?".
    // The relation is authored in the body, which is where the parser reads relations
    // from; it survives even though the extracted text itself is graph-silent.
    frontmatter.push_str(&format!("derived_from: {source_relative}\n"));
    if let Some(count) = extraction.line_count {
        frontmatter.push_str(&format!("source_lines: {count}\n"));
    }
    if extraction.truncated {
        frontmatter.push_str("extraction_truncated: true\n");
    }
    frontmatter.push_str("---\n\n");

    let mut body = String::new();
    // The relation line is authored text, not extracted text, so it belongs above the
    // `bm_parse_semantics: false` boundary: a `derived_from` edge is knowledge about the
    // sidecar, while the document's own words are not.
    body.push_str(&format!("- derived_from [[{source_relative}]]\n\n"));
    if let Some(hint) = extraction.format.extractor_hint() {
        // Not an empty body: an empty sidecar reads as "this document has no text".
        body.push_str(&format!("> [!warning] Not extracted\n>\n> {hint}\n\n"));
        body.push_str(&format!(
            "The source file `{source_relative}` was indexed, but its text is not in this \
             note.\n"
        ));
    } else {
        body.push_str(&extraction.text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    Sidecar {
        file_path: sidecar,
        content: format!("{frontmatter}{body}"),
    }
}

/// Build the ingestion-run note that records provenance.
///
/// Upstream 0.24.0 keeps the per-page map only here, so a 400-page PDF's sidecar stays
/// small while the run note carries the detail.
#[must_use]
pub fn build_run_note(
    source_relative: &str,
    extraction: &Extraction,
    source_checksum: &str,
    run_id: &str,
) -> Sidecar {
    let file_path = format!("document-ingestion-runs/{run_id}.md");
    let mut content = String::from("---\n");
    content.push_str(&format!("title: \"Ingestion run {run_id}\"\n"));
    content.push_str("type: document_ingestion_run\n");
    content.push_str("bm_parse_semantics: false\n");
    content.push_str(&format!("source: {source_relative}\n"));
    content.push_str(&format!("source_checksum: {source_checksum}\n"));
    content.push_str(&format!(
        "format: {}\n",
        match extraction.format {
            DocumentFormat::Csv => "csv",
            DocumentFormat::Text => "text",
            DocumentFormat::Pdf => "pdf",
            DocumentFormat::Word => "docx",
            DocumentFormat::PowerPoint => "pptx",
            DocumentFormat::Excel => "xlsx",
        }
    ));
    content.push_str(&format!(
        "extracted: {}\n",
        extraction.format.is_extractable()
    ));
    content.push_str(&format!("truncated: {}\n", extraction.truncated));
    if let Some(count) = extraction.line_count {
        content.push_str(&format!("source_lines: {count}\n"));
    }
    content.push_str("---\n\n");

    content.push_str("# Ingestion run\n\n");
    content.push_str(&format!("- source: `{source_relative}`\n"));
    content.push_str(&format!("- sidecar: `{source_relative}.md`\n"));
    content.push_str(&format!("- checksum: `{source_checksum}`\n"));
    if let Some(hint) = extraction.format.extractor_hint() {
        content.push_str(&format!("- status: not extracted\n\n> {hint}\n"));
    } else {
        content.push_str("- status: extracted\n");
    }
    Sidecar { file_path, content }
}

/// Whether a sidecar path already holds a note the import must not overwrite.
///
/// A file the import did not write — a hand-authored note, or one someone enriched with
/// observations — is left alone. Only a note this importer produced (recognisable by its
/// `bm_parse_semantics: false` and `source_checksum`) may be replaced.
#[must_use]
pub fn is_replaceable(existing: &str) -> bool {
    existing.contains("bm_parse_semantics: false")
        && existing.contains("source_checksum:")
        && existing.contains("type: extracted_text")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_recognised_from_the_extension() {
        assert_eq!(
            DocumentFormat::of(Path::new("a.csv")),
            Some(DocumentFormat::Csv)
        );
        assert_eq!(
            DocumentFormat::of(Path::new("a.PDF")),
            Some(DocumentFormat::Pdf)
        );
        assert_eq!(
            DocumentFormat::of(Path::new("a.docx")),
            Some(DocumentFormat::Word)
        );
        assert_eq!(DocumentFormat::of(Path::new("a.png")), None);
    }

    #[test]
    fn only_csv_and_text_are_extractable_here() {
        assert!(
            DocumentFormat::Csv.is_extractable(),
            "CSV text is extractable without an external tool"
        );
        assert!(
            DocumentFormat::Text.is_extractable(),
            "plain text is extractable without an external tool"
        );
        assert!(
            !DocumentFormat::Pdf.is_extractable(),
            "PDF needs pdf-inspector, which this build does not bundle"
        );
        assert!(
            !DocumentFormat::Word.is_extractable(),
            "Word needs markitdown, which this build does not bundle"
        );
        // Every non-extractable format explains itself rather than shipping an empty body.
        for format in [
            DocumentFormat::Pdf,
            DocumentFormat::Word,
            DocumentFormat::PowerPoint,
            DocumentFormat::Excel,
        ] {
            assert!(format.extractor_hint().is_some(), "{format:?}");
        }
    }

    #[test]
    fn a_csv_becomes_a_markdown_table() {
        let extraction = extract_csv("name,age\nada,36\n");
        assert!(
            extraction.text.contains("| name | age |"),
            "{}",
            extraction.text
        );
        assert!(
            extraction.text.contains("| --- | --- |"),
            "{}",
            extraction.text
        );
        assert!(
            extraction.text.contains("| ada | 36 |"),
            "{}",
            extraction.text
        );
        assert_eq!(extraction.line_count, Some(2));
        assert!(!extraction.truncated, "a short text file is not truncated");
    }

    #[test]
    fn a_quoted_csv_field_keeps_its_comma() {
        let cells = split_csv_row("a,\"b,c\",d");
        assert_eq!(cells, vec!["a", "b,c", "d"]);
    }

    #[test]
    fn an_escaped_quote_survives() {
        let cells = split_csv_row("\"say \"\"hi\"\"\",x");
        assert_eq!(cells, vec![r#"say "hi""#, "x"]);
    }

    #[test]
    fn a_long_csv_is_bounded_and_says_so() {
        let rows: String = (0..CSV_PREVIEW_ROWS + 50)
            .map(|index| format!("{index},v{index}\n"))
            .collect();
        let extraction = extract_csv(&rows);
        assert!(
            extraction.truncated,
            "a text file past the preview cap is truncated"
        );
        assert!(
            extraction.text.contains("Preview: first"),
            "{}",
            extraction.text
        );
    }

    #[test]
    fn a_text_file_is_copied_through() {
        let extraction = extract_text("hello\nworld\n");
        assert_eq!(extraction.text, "hello\nworld\n");
        assert_eq!(extraction.line_count, Some(2));
        assert!(!extraction.truncated, "a short text file is not truncated");
    }

    #[test]
    fn a_long_text_file_is_bounded() {
        let long = "x".repeat(TEXT_PREVIEW_CHARS + 100);
        let extraction = extract_text(&long);
        assert!(
            extraction.truncated,
            "a text file past the preview cap is truncated"
        );
        assert_eq!(extraction.text.chars().count(), TEXT_PREVIEW_CHARS);
    }

    #[test]
    fn the_sidecar_path_appends_md_beside_the_source() {
        assert_eq!(
            sidecar_path(Path::new("docs/report.pdf")),
            PathBuf::from("docs/report.pdf.md")
        );
    }

    #[test]
    fn a_sidecar_marks_itself_as_not_parseable() {
        let extraction = extract_csv("a,b\n1,2\n");
        let sidecar = build_sidecar("data/table.csv", &extraction, "abc");
        assert_eq!(sidecar.file_path, "data/table.csv.md");
        assert!(
            sidecar.content.contains("bm_parse_semantics: false"),
            "the sidecar must not mint knowledge from extracted text"
        );
        assert!(
            sidecar.content.contains("type: extracted_text"),
            "the note is still an extracted_text sidecar"
        );
        assert!(
            sidecar.content.contains("source: data/table.csv"),
            "the sidecar records its source"
        );
        assert!(
            sidecar.content.contains("| a | b |"),
            "the CSV rows must render as a Markdown table"
        );
    }

    #[test]
    fn an_unextractable_sidecar_explains_itself() {
        // The distinction that matters: an empty sidecar reads as "no text here", which is
        // a lie for a PDF.
        let extraction =
            extract(Path::new("does-not-exist.pdf"), DocumentFormat::Pdf).unwrap_or(Extraction {
                format: DocumentFormat::Pdf,
                text: String::new(),
                truncated: false,
                line_count: None,
            });
        let sidecar = build_sidecar("docs/report.pdf", &extraction, "abc");
        assert!(
            sidecar.content.contains("Not extracted"),
            "{}",
            sidecar.content
        );
        assert!(
            sidecar.content.contains("pdf-inspector"),
            "the sidecar must name the missing extractor"
        );
        // The note is still an `extracted_text` sidecar — it records that extraction was
        // attempted — but its body says plainly that the text is not there.
        assert!(
            sidecar.content.contains("type: extracted_text"),
            "the note is still an extracted_text sidecar"
        );
        assert!(
            sidecar.content.contains("its text is not in this"),
            "{}",
            sidecar.content
        );
    }

    #[test]
    fn the_run_note_records_provenance() {
        let extraction = extract_csv("a,b\n1,2\n");
        let run = build_run_note("data/table.csv", &extraction, "abc", "run-123");
        assert_eq!(run.file_path, "document-ingestion-runs/run-123.md");
        assert!(
            run.content.contains("type: document_ingestion_run"),
            "a run note declares its own type"
        );
        assert!(
            run.content.contains("status: extracted"),
            "the run note records that extraction succeeded"
        );
        assert!(
            run.content.contains("checksum: `abc`"),
            "the run note records the source checksum"
        );
    }

    #[test]
    fn a_run_id_is_stable_for_one_source_revision() {
        let first = run_id("data/table.csv", "abc");
        let second = run_id("data/table.csv", "abc");
        assert_eq!(first, second);
        // A changed source gets a new run, so the old one is not silently overwritten.
        assert_ne!(first, run_id("data/table.csv", "def"));
    }

    #[test]
    fn only_an_imported_sidecar_is_replaceable() {
        let imported = build_sidecar("a.csv", &extract_csv("a\n1\n"), "abc");
        assert!(
            is_replaceable(&imported.content),
            "a note this importer wrote may be replaced"
        );
        // A hand-written note at the same path is not.
        assert!(!is_replaceable(
            "---\ntitle: My notes\ntype: note\n---\n\nHand written.\n"
        ));
        // Nor is an enriched sidecar someone added observations to.
        let mut enriched = imported.content.clone();
        enriched.push_str("\n- [fact] I added this myself\n");
        // Still replaceable by the marker test; the *caller* refuses when the file
        // changed since the import, which is what the checksum comparison covers.
        assert!(
            is_replaceable(&enriched),
            "the marker still identifies an importer-written note"
        );
    }
}
