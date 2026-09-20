use std::fs;
use std::path::PathBuf;

use chrono::DateTime;
use chrono_tz::Europe::Copenhagen;
use serde::Deserialize;
use teamup_shift_sync_core::{
    classify_source_title, parse_sps_instructions, MeetingCategory, ParseIssueCode, SourceComment,
    SourceShift, SourceTitle, NOTES_SOURCE_ID,
};

fn shift(starts_at: &str, ends_at: &str, notes: &str, comment: Option<&str>) -> SourceShift {
    SourceShift {
        calendar_id: "calendar".into(),
        event_id: "event".into(),
        occurrence_id: starts_at.into(),
        title: "Shift".into(),
        helper_key: "helper".into(),
        starts_at: DateTime::parse_from_rfc3339(starts_at).unwrap(),
        ends_at: DateTime::parse_from_rfc3339(ends_at).unwrap(),
        notes: notes.into(),
        comments: comment
            .map(|text| SourceComment {
                id: "comment-1".into(),
                text: text.into(),
                updated_at: None,
            })
            .into_iter()
            .collect(),
        recurrence_start: None,
        source_version: None,
    }
}

fn ordinary_shift(notes: &str, comment: Option<&str>) -> SourceShift {
    shift(
        "2026-09-14T07:30:00+02:00",
        "2026-09-14T15:00:00+02:00",
        notes,
        comment,
    )
}

#[test]
fn parses_description_and_comment_while_preserving_split_intervals() {
    let result = parse_sps_instructions(&ordinary_shift("uni 8-10", Some("uni 13-14")), Copenhagen);

    assert!(result.issues.is_empty());
    assert_eq!(result.relevant_source_ids, [NOTES_SOURCE_ID, "comment-1"]);
    assert_eq!(result.intervals.len(), 2);
    assert_eq!(result.intervals[0].key, "notes:0");
    assert_eq!(result.intervals[0].interval.hours(), 2.0);
    assert_eq!(result.intervals[1].interval.hours(), 1.0);
    assert_eq!(
        result.intervals[1].interval.starts_at.to_rfc3339(),
        "2026-09-14T13:00:00+02:00"
    );
}

#[test]
fn weekday_spellings_resolve_the_second_day_of_a_shift() {
    for spelling in ["fre", "fre.", "FRE", "fredag"] {
        let result = parse_sps_instructions(
            &shift(
                "2026-09-17T13:00:00+02:00",
                "2026-09-18T16:00:00+02:00",
                &format!("uni 12-14 {spelling}"),
                None,
            ),
            Copenhagen,
        );

        assert!(result.issues.is_empty(), "spelling: {spelling}");
        assert_eq!(
            result.intervals[0].interval.starts_at.to_rfc3339(),
            "2026-09-18T12:00:00+02:00"
        );
    }
}

#[test]
fn weekday_suffix_applies_to_each_split_interval() {
    let result = parse_sps_instructions(
        &shift(
            "2026-09-17T13:00:00+02:00",
            "2026-09-18T16:00:00+02:00",
            "UNI 8-10 & 12-14 FREDAG",
            None,
        ),
        Copenhagen,
    );

    assert!(result.issues.is_empty());
    assert_eq!(result.intervals.len(), 2);
    assert!(result.intervals.iter().all(|interval| interval
        .interval
        .starts_at
        .format("%d")
        .to_string()
        == "18"));
}

#[test]
fn weekday_never_guesses_or_overrides_shift_bounds() {
    let cases = [
        (
            "2026-09-25T16:00:00+02:00",
            "uni 12-14 fredag",
            ParseIssueCode::AmbiguousUniWeekday,
        ),
        (
            "2026-09-18T16:00:00+02:00",
            "uni 12-14 lørdag",
            ParseIssueCode::UniWeekdayOutsideShift,
        ),
        (
            "2026-09-18T13:00:00+02:00",
            "uni 12-14 fredag",
            ParseIssueCode::UniOutsideShift,
        ),
        (
            "2026-09-18T16:00:00+02:00",
            "uni 2026-09-17 14-15 fredag",
            ParseIssueCode::ConflictingUniDate,
        ),
    ];

    for (ends_at, instruction, expected) in cases {
        let result = parse_sps_instructions(
            &shift("2026-09-17T13:00:00+02:00", ends_at, "", Some(instruction)),
            Copenhagen,
        );
        assert!(result.intervals.is_empty(), "instruction: {instruction}");
        assert_eq!(
            result.issues[0].code, expected,
            "instruction: {instruction}"
        );
    }
}

#[test]
fn an_undated_overnight_instruction_is_ambiguous() {
    let result = parse_sps_instructions(
        &shift(
            "2026-09-14T22:00:00+02:00",
            "2026-09-15T07:00:00+02:00",
            "",
            Some("uni 23-1"),
        ),
        Copenhagen,
    );

    assert!(result.intervals.is_empty());
    assert_eq!(result.issues[0].code, ParseIssueCode::AmbiguousUniDate);
}

#[test]
fn an_explicit_date_supports_an_overnight_interval() {
    let result = parse_sps_instructions(
        &shift(
            "2026-09-14T22:00:00+02:00",
            "2026-09-15T07:00:00+02:00",
            "",
            Some("uni 2026-09-14 23-1"),
        ),
        Copenhagen,
    );

    assert!(result.issues.is_empty());
    assert_eq!(
        result.intervals[0].interval.ends_at.to_rfc3339(),
        "2026-09-15T01:00:00+02:00"
    );
}

#[test]
fn nonexistent_and_ambiguous_dst_times_are_not_guessed() {
    let cases = [
        (
            "2026-03-29T00:00:00+01:00",
            "2026-03-29T05:00:00+02:00",
            ParseIssueCode::NonexistentLocalTime,
        ),
        (
            "2026-10-25T00:00:00+02:00",
            "2026-10-25T05:00:00+01:00",
            ParseIssueCode::AmbiguousLocalTime,
        ),
    ];

    for (starts_at, ends_at, expected) in cases {
        let result = parse_sps_instructions(
            &shift(starts_at, ends_at, "", Some("uni 2:30-4")),
            Copenhagen,
        );
        assert!(result.intervals.is_empty());
        assert_eq!(result.issues[0].code, expected);
    }
}

#[test]
fn duplicate_intervals_are_planned_once_and_overlaps_require_review() {
    let duplicate =
        parse_sps_instructions(&ordinary_shift("uni 8-10", Some("uni 8-10")), Copenhagen);
    assert_eq!(duplicate.intervals.len(), 1);
    assert_eq!(
        duplicate.issues[0].code,
        ParseIssueCode::DuplicateUniInterval
    );

    let overlap = parse_sps_instructions(&ordinary_shift("", Some("uni 8-11 & 10-12")), Copenhagen);
    assert_eq!(overlap.intervals.len(), 1);
    assert_eq!(
        overlap.issues[0].code,
        ParseIssueCode::OverlappingUniIntervals
    );
}

#[test]
fn invalid_instructions_become_review_issues() {
    let bad_syntax = parse_sps_instructions(
        &ordinary_shift("", Some("please add uni tomorrow")),
        Copenhagen,
    );
    assert_eq!(
        bad_syntax.issues[0].code,
        ParseIssueCode::UnsupportedUniSyntax
    );

    let bad_interval = parse_sps_instructions(&ordinary_shift("", Some("uni 25-26")), Copenhagen);
    assert_eq!(
        bad_interval.issues[0].code,
        ParseIssueCode::InvalidUniInterval
    );
}

#[test]
fn source_titles_preserve_reminder_and_meeting_rules() {
    assert_eq!(
        classify_source_title(" Husk at checke vagtplanen på AXP "),
        SourceTitle::Reminder
    );
    assert_eq!(classify_source_title("Husk at checker"), SourceTitle::Shift);
    assert_eq!(
        classify_source_title(" p-møde "),
        SourceTitle::Meeting(MeetingCategory {
            name: "Vagtmøde",
            code: "4:1",
        })
    );
}

#[derive(Deserialize)]
struct RepresentativeFixture {
    source_shifts: Vec<SourceShift>,
}

#[test]
fn reads_the_existing_representative_fixture() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/representative-week.json");
    let fixture: RepresentativeFixture =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

    let result = parse_sps_instructions(&fixture.source_shifts[0], Copenhagen);
    assert!(result.issues.is_empty());
    assert_eq!(result.intervals.len(), 2);
    assert_eq!(result.intervals[0].source_id, "comment-split");
}
