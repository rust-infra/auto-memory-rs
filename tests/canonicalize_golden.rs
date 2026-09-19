//! Unit tests for the golden-comparison helpers in `tests/common`.

mod common;

use common::{canonicalize_text, canonicalize_timestamps, canonicalize_uuids, golden_dir};

#[test]
fn replaces_uuids_only_when_well_formed() {
    let src = "id=123e4567-e89b-12d3-a456-426614174000 other=123e4567-e89b-12d3";
    assert_eq!(
        canonicalize_uuids(src),
        "id=<uuid> other=123e4567-e89b-12d3"
    );
}

#[test]
fn replaces_timestamps() {
    let src = "created 2026-01-02T03:04:05.123Z and 2026-01-02 03:04:05+08:00";
    let got = canonicalize_timestamps(src);
    assert_eq!(got, "created <timestamp> and <timestamp>");
}

#[test]
fn canonicalize_text_trims_trailing_whitespace() {
    let src = "a   \r\nb\t\r\n";
    assert_eq!(canonicalize_text(src), "a\nb");
}

#[test]
fn golden_dir_points_at_tests_golden() {
    assert!(golden_dir().ends_with("tests/golden"));
}
