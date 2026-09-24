//! iCal feeds as exported by real calendar services, read one week at a time.

use chrono::{DateTime, FixedOffset, NaiveDate};
use chrono_tz::Europe::Copenhagen;
use teamup_shift_sync_core::ical::{parse_feed, HelperRule, IssueKind, ParsedFeed, TitlePart};
use teamup_shift_sync_core::standard_time::StandardTimes;

const GOOGLE: &str = include_str!("ical/google.ics");
const OUTLOOK: &str = include_str!("ical/outlook.ics");
const SHARED: &str = include_str!("ical/shared.ics");

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
}

fn at(text: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(text).unwrap()
}

/// The week DST ends: Monday 19 to Sunday 25 October 2026.
fn week(text: &str, rule: &HelperRule, standard: &StandardTimes) -> ParsedFeed {
    parse_feed(
        text,
        "ical-feed",
        rule,
        Copenhagen,
        day(19),
        day(25),
        standard,
    )
    .unwrap()
}

fn standard() -> StandardTimes {
    StandardTimes {
        everyday: "6-22".into(),
        ..Default::default()
    }
}

fn times(parsed: &ParsedFeed) -> Vec<(String, String, String)> {
    parsed
        .shifts
        .iter()
        .map(|shift| {
            (
                shift.title.clone(),
                shift.starts_at.to_rfc3339(),
                shift.ends_at.to_rfc3339(),
            )
        })
        .collect()
}

#[test]
fn a_google_export_reads_overrides_exceptions_all_day_and_dst() {
    let parsed = week(GOOGLE, &HelperRule::Feed, &standard());
    assert_eq!(parsed.name.as_deref(), Some("Anna vagter"));
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    assert_eq!(
        times(&parsed),
        [
            // The moved Monday replaces the series' 8-16, even though its
            // SEQUENCE differs from the series.
            (
                "Vagt",
                "2026-10-19T10:00:00+02:00",
                "2026-10-19T18:00:00+02:00"
            ),
            // All-day uses the standard time.
            (
                "Vagt",
                "2026-10-21T06:00:00+02:00",
                "2026-10-21T22:00:00+02:00"
            ),
            // UTC times land in the local zone.
            (
                "Vagt",
                "2026-10-22T08:00:00+02:00",
                "2026-10-22T16:00:00+02:00"
            ),
            // The cancelled Friday is gone. The night shift ends after the
            // clocks go back, so it is eleven hours long.
            (
                "Nattevagt",
                "2026-10-24T22:00:00+02:00",
                "2026-10-25T08:00:00+01:00"
            ),
        ]
        .map(|(title, start, end)| (title.to_owned(), start.to_owned(), end.to_owned()))
    );
    for shift in &parsed.shifts {
        assert_eq!(shift.helper_key, "ical-feed");
        assert_eq!(shift.calendar_id, "ical-feed");
    }
    let moved = &parsed.shifts[0];
    assert_eq!(moved.notes, "Byttet med Bo, starter senere");
    // The occurrence keeps the identity of the slot it replaced.
    assert_eq!(
        moved.recurrence_start,
        Some(at("2026-10-19T08:00:00+02:00"))
    );
    assert!(moved.occurrence_id.ends_with("-20261019T060000Z"));
    assert!(parsed.shifts[1].standard_time);
    assert_eq!(parsed.shifts[1].notes, "Husk nøgler");
    // A single event's occurrence is the event itself.
    assert_eq!(parsed.shifts[2].occurrence_id, parsed.shifts[2].event_id);
    assert_eq!(parsed.shifts[2].recurrence_start, None);
}

#[test]
fn the_same_series_is_expanded_in_any_week() {
    let parsed = parse_feed(
        GOOGLE,
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        day(5),
        day(11),
        &standard(),
    )
    .unwrap();
    assert_eq!(
        times(&parsed),
        [
            (
                "Vagt",
                "2026-10-05T08:00:00+02:00",
                "2026-10-05T16:00:00+02:00"
            ),
            (
                "Aftenvagt",
                "2026-10-09T16:00:00+02:00",
                "2026-10-09T22:00:00+02:00"
            ),
        ]
        .map(|(title, start, end)| (title.to_owned(), start.to_owned(), end.to_owned()))
    );
    // EXDATE removes the next Monday.
    let parsed = parse_feed(
        GOOGLE,
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        day(12),
        day(18),
        &standard(),
    )
    .unwrap();
    assert_eq!(parsed.shifts.len(), 1);
    assert_eq!(parsed.shifts[0].title, "Aftenvagt");
    // Winter time: the open-ended series keeps its local hours.
    let parsed = parse_feed(
        GOOGLE,
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        NaiveDate::from_ymd_opt(2027, 3, 1).unwrap(),
        NaiveDate::from_ymd_opt(2027, 3, 7).unwrap(),
        &standard(),
    )
    .unwrap();
    assert_eq!(
        times(&parsed),
        [(
            "Vagt",
            "2027-03-01T08:00:00+01:00",
            "2027-03-01T16:00:00+01:00"
        )]
        .map(|(title, start, end)| (title.to_owned(), start.to_owned(), end.to_owned()))
    );
}

#[test]
fn an_outlook_export_resolves_windows_time_zones() {
    let parsed = week(OUTLOOK, &HelperRule::Feed, &standard());
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    // This week's Tuesday is excluded; Thursday's event is read in CEST.
    assert_eq!(
        times(&parsed),
        [(
            "Eftermiddag",
            "2026-10-22T12:00:00+02:00",
            "2026-10-22T20:00:00+02:00"
        )]
        .map(|(title, start, end)| (title.to_owned(), start.to_owned(), end.to_owned()))
    );
    // The series still runs the week before, with its folded UID intact.
    let before = parse_feed(
        OUTLOOK,
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        day(12),
        day(18),
        &standard(),
    )
    .unwrap();
    assert_eq!(
        times(&before),
        [(
            "Morgenvagt",
            "2026-10-13T07:00:00+02:00",
            "2026-10-13T15:00:00+02:00"
        )]
        .map(|(title, start, end)| (title.to_owned(), start.to_owned(), end.to_owned()))
    );
    assert_eq!(before.shifts[0].notes, "Sps 9-11");
    // COUNT=10 from 1 September ends on 3 November.
    let after = parse_feed(
        OUTLOOK,
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        &standard(),
    )
    .unwrap();
    assert!(after.shifts.is_empty());
}

fn before_dash() -> HelperRule {
    HelperRule::Title {
        part: TitlePart::Before,
        separator: "-".into(),
    }
}

#[test]
fn a_shared_feed_names_helpers_in_titles() {
    let parsed = week(SHARED, &before_dash(), &standard());
    // Anna written two ways is one helper; Carl's shift is outside the week.
    assert_eq!(
        parsed.helpers.into_iter().collect::<Vec<_>>(),
        [("anna", "Anna"), ("bo", "Bo")].map(|(key, name)| (key.to_owned(), name.to_owned()))
    );
    let shifts: Vec<_> = parsed
        .shifts
        .iter()
        .map(|shift| (shift.helper_key.as_str(), shift.title.as_str()))
        .collect();
    // The rest of the title is the shift's title, so P-møde is a meeting.
    // Without a separator the whole title is the name. The reminder is skipped.
    assert_eq!(
        shifts,
        [("anna", "Vagt"), ("anna", "P-møde"), ("bo", "Vagt")]
    );
    assert_eq!(parsed.shifts[0].notes, "Sps 10-12");
    assert_eq!(parsed.issues.len(), 1);
    assert_eq!(parsed.issues[0].kind, IssueKind::MissingHelper);
    assert_eq!(
        parsed.issues[0].message(),
        "Vagten d. 22/10 mangler en hjælper i titlen."
    );
}

#[test]
fn title_rules_split_names_before_after_or_whole() {
    let after = HelperRule::Title {
        part: TitlePart::After,
        separator: ":".into(),
    };
    assert_eq!(
        after.split("Vagt: Anna  Berg"),
        (Some("Anna Berg".into()), "Vagt")
    );
    assert_eq!(after.split("Anna"), (Some("Anna".into()), ""));
    assert_eq!(after.split("Vagt:"), (None, "Vagt"));
    let whole = HelperRule::Title {
        part: TitlePart::Whole,
        separator: String::new(),
    };
    assert_eq!(whole.split(" Anna "), (Some("Anna".into()), ""));
    whole.validate().unwrap();
    assert!(HelperRule::Title {
        part: TitlePart::Before,
        separator: " ".into()
    }
    .validate()
    .is_err());
}

fn feed(events: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
}

#[test]
fn events_that_cannot_be_transferred_as_written_are_issues_for_their_week() {
    let text = feed(concat!(
        // Multi-day all-day.
        "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261019\r\nDTEND;VALUE=DATE:20261021\r\nSUMMARY:Vagt\r\nEND:VEVENT\r\n",
        // 02:30 happens twice on 25 October.
        "BEGIN:VEVENT\r\nUID:b\r\nDTSTART;TZID=Europe/Copenhagen:20261025T023000\r\nDTEND;TZID=Europe/Copenhagen:20261025T090000\r\nSUMMARY:Vagt\r\nEND:VEVENT\r\n",
        // Ends when it starts.
        "BEGIN:VEVENT\r\nUID:c\r\nDTSTART:20261022T060000Z\r\nDTEND:20261022T060000Z\r\nSUMMARY:Vagt\r\nEND:VEVENT\r\n",
        // All-day on a weekday without a standard.
        "BEGIN:VEVENT\r\nUID:d\r\nDTSTART;VALUE=DATE:20261023\r\nSUMMARY:Vagt\r\nEND:VEVENT\r\n",
        // Next week: not this week's problem.
        "BEGIN:VEVENT\r\nUID:e\r\nDTSTART;VALUE=DATE:20261027\r\nDTEND;VALUE=DATE:20261029\r\nSUMMARY:Vagt\r\nEND:VEVENT\r\n",
    ));
    let standard = StandardTimes {
        everyday: "6-22".into(),
        weekdays: [("fri".to_owned(), None)].into(),
    };
    let parsed = week(&text, &HelperRule::Feed, &standard);
    assert!(parsed.shifts.is_empty());
    let mut kinds: Vec<_> = parsed
        .issues
        .iter()
        .map(|issue| (issue.kind, issue.date))
        .collect();
    kinds.sort_by_key(|(_, date)| *date);
    assert_eq!(
        kinds,
        [
            (IssueKind::MultiDay, Some(day(19))),
            (IssueKind::NoDuration, Some(day(22))),
            (IssueKind::NoStandardTime, Some(day(23))),
            (IssueKind::AmbiguousTime, Some(day(25))),
        ]
    );
}

#[test]
fn a_repeat_on_a_skipped_clock_time_is_an_issue_for_that_day_only() {
    // Every Sunday at 02:30; on 29 March 2026 that time does not exist.
    let text = feed(concat!(
        "BEGIN:VEVENT\r\nUID:weekly\r\nDTSTART;TZID=Europe/Copenhagen:20260301T023000\r\n",
        "DTEND;TZID=Europe/Copenhagen:20260301T060000\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Nat\r\nEND:VEVENT\r\n",
    ));
    let read = |from: u32, to: u32| {
        parse_feed(
            &text,
            "ical-feed",
            &HelperRule::Feed,
            Copenhagen,
            NaiveDate::from_ymd_opt(2026, 3, from).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, to).unwrap(),
            &standard(),
        )
        .unwrap()
    };
    let spring = read(23, 29);
    assert!(spring.shifts.is_empty());
    assert_eq!(spring.issues.len(), 1);
    assert_eq!(spring.issues[0].kind, IssueKind::AmbiguousTime);
    let earlier = read(16, 22);
    assert!(earlier.issues.is_empty());
    assert_eq!(earlier.shifts.len(), 1);
}

#[test]
fn text_that_is_not_a_calendar_is_refused() {
    assert!(parse_feed(
        "<!DOCTYPE html><html></html>",
        "ical-feed",
        &HelperRule::Feed,
        Copenhagen,
        day(19),
        day(25),
        &standard(),
    )
    .is_err());
}
