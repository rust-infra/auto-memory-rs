//! Bounded line reads: `read_note(start_line, end_line)` and `cat`'s line range.
//!
//! Upstream 0.24.0 (#1501) lets a caller read one range of a note instead of the whole
//! body. Two properties matter for an agent:
//!
//! - every line comes back **numbered**, so the caller can cite and continue;
//! - the reply names **the next range to read**, so a long note is walked without
//!   guessing.
//!
//! The range is 1-based and inclusive, which is how a human reads `sed -n '120,180p'`.
//! An empty range is a caller error rather than an empty reply: silently answering with
//! nothing is what makes an agent conclude the note is empty.

use crate::error::{Error, Result};

/// The default number of lines a bounded read returns when only `start_line` is given.
///
/// Sized so one read fits comfortably in a model's context while still being a useful
/// chunk of a long note. `end_line` overrides it.
pub const DEFAULT_LINE_WINDOW: usize = 120;

/// One bounded read of a note's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRange {
    /// The first line returned, 1-based.
    pub start: usize,
    /// The last line returned, 1-based and inclusive.
    pub end: usize,
    /// Total number of lines in the document.
    pub total: usize,
    /// The rendered, numbered lines.
    pub body: String,
}

impl LineRange {
    /// The range a caller should request next, or `None` at the end of the document.
    #[must_use]
    pub fn next_range(&self) -> Option<(usize, usize)> {
        (self.end < self.total).then(|| {
            let start = self.end + 1;
            let width = self.end - self.start + 1;
            (start, (start + width - 1).min(self.total))
        })
    }

    /// Whether this range covers the whole document.
    #[must_use]
    pub fn is_whole_document(&self) -> bool {
        self.start == 1 && self.end == self.total
    }
}

/// Read lines `[start_line, end_line]` out of `content`.
///
/// `start_line` defaults to 1 and `end_line` to `start + DEFAULT_LINE_WINDOW - 1`.
/// `end_line` past the end of the document is clamped, because "read to the end" is a
/// normal request; a `start_line` past the end is an error that names the document's
/// length, which is the reference's behavior (#1705: `start_line 500 is past the end of
/// the document (17 lines)`).
pub fn read_lines(
    content: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<LineRange> {
    let lines: Vec<&str> = content.split('\n').collect();
    // A trailing newline produces one empty final element; it is not a line of the
    // document, and counting it would make `total` disagree with every editor.
    let total = if content.is_empty() {
        0
    } else if content.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    let start = start_line.unwrap_or(1).max(1);
    if start > total.max(1) && total > 0 {
        return Err(Error::InvalidArgument {
            message: format!("start_line {start} is past the end of the document ({total} lines)"),
        });
    }
    if let Some(end) = end_line
        && end < start
    {
        return Err(Error::InvalidArgument {
            message: format!("end_line {end} is before start_line {start}"),
        });
    }
    let end = end_line
        .unwrap_or_else(|| start + DEFAULT_LINE_WINDOW - 1)
        .min(total.max(start));

    let mut body = String::new();
    for (offset, line) in lines.iter().enumerate().take(end).skip(start - 1) {
        // `{:>4}` keeps the numbers aligned for any document under 10k lines, and the
        // pipe keeps a leading `-` in the note from reading as part of the number.
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("{:>4} | {line}", offset + 1));
    }
    Ok(LineRange {
        start,
        end,
        total,
        body,
    })
}

/// Render the whole bounded-read reply, including the next-range hint.
#[must_use]
pub fn render_line_range(identifier: &str, range: &LineRange) -> String {
    let mut out = format!(
        "# {identifier} (lines {}-{} of {})\n\n{}\n",
        range.start, range.end, range.total, range.body
    );
    match range.next_range() {
        Some((start, end)) => out.push_str(&format!(
            "\n*Next: read_note(identifier=\"{identifier}\", start_line={start}, end_line={end})*"
        )),
        None => out.push_str("\n*End of document.*"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "line one\nline two\nline three\nline four\nline five\n";

    #[test]
    fn a_whole_document_read_numbers_every_line() {
        let range = read_lines(NOTE, None, None).expect("range");
        assert_eq!((range.start, range.end, range.total), (1, 5, 5));
        assert!(range.is_whole_document());
        assert!(range.body.starts_with("   1 | line one"), "{}", range.body);
        assert!(range.body.ends_with("   5 | line five"), "{}", range.body);
        // The trailing newline is not a sixth line.
        assert_eq!(range.next_range(), None);
    }

    #[test]
    fn a_middle_range_names_the_next_one() {
        let range = read_lines(NOTE, Some(2), Some(3)).expect("range");
        assert_eq!((range.start, range.end, range.total), (2, 3, 5));
        assert_eq!(range.body, "   2 | line two\n   3 | line three");
        assert_eq!(range.next_range(), Some((4, 5)));
        assert!(!range.is_whole_document());
    }

    #[test]
    fn end_line_past_the_document_is_clamped() {
        let range = read_lines(NOTE, Some(4), Some(999)).expect("range");
        assert_eq!((range.start, range.end), (4, 5));
        assert_eq!(range.next_range(), None);
    }

    #[test]
    fn a_start_past_the_end_is_an_error_naming_the_length() {
        let error = read_lines(NOTE, Some(500), None).expect_err("out of range");
        assert_eq!(
            error.to_string(),
            "start_line 500 is past the end of the document (5 lines)"
        );
    }

    #[test]
    fn an_inverted_range_is_refused() {
        let error = read_lines(NOTE, Some(4), Some(2)).expect_err("inverted");
        assert!(error.to_string().contains("before start_line"), "{error}");
    }

    #[test]
    fn only_start_line_reads_a_default_window() {
        let long: String = (1..=500)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let range = read_lines(&long, Some(1), None).expect("range");
        assert_eq!(range.end, DEFAULT_LINE_WINDOW);
        assert_eq!(range.total, 500);
        assert_eq!(range.next_range(), Some((121, 240)));
    }

    #[test]
    fn an_empty_document_reads_as_one_empty_range() {
        let range = read_lines("", None, None).expect("range");
        assert_eq!((range.start, range.end, range.total), (1, 1, 0));
        assert_eq!(range.body, "   1 | ");
    }

    #[test]
    fn the_render_names_the_identifier_and_the_next_call() {
        let range = read_lines(NOTE, Some(1), Some(2)).expect("range");
        let text = render_line_range("notes/simple", &range);
        assert!(text.contains("# notes/simple (lines 1-2 of 5)"), "{text}");
        assert!(
            text.contains(r#"read_note(identifier="notes/simple", start_line=3, end_line=4)"#),
            "{text}"
        );
    }

    #[test]
    fn a_note_that_does_not_end_in_a_newline_counts_its_last_line() {
        let range = read_lines("a\nb", None, None).expect("range");
        assert_eq!(range.total, 2);
        assert_eq!(range.body, "   1 | a\n   2 | b");
    }
}
