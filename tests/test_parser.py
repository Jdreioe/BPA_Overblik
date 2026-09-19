from __future__ import annotations

import unittest
from zoneinfo import ZoneInfo

from helpers import source_shift

from teamup_shift_sync.parser import NOTES_SOURCE_ID, parse_sps_instructions


class SpsParserTests(unittest.TestCase):
    timezone = ZoneInfo("Europe/Copenhagen")

    def test_instruction_in_event_description_is_parsed(self):
        """The real calendar writes 'uni' in the description, not a comment."""
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-09-17T13:00:00+02:00",
                ends_at="2026-09-18T16:00:00+02:00",
                comment=None,
                notes="uni 12-14 fredag",
            ),
            self.timezone,
        )
        self.assertEqual((), result.issues)
        self.assertEqual((NOTES_SOURCE_ID,), result.relevant_source_ids)
        self.assertEqual(f"{NOTES_SOURCE_ID}:0", result.intervals[0].key)
        self.assertEqual(
            "2026-09-18T12:00:00+02:00",
            result.intervals[0].interval.starts_at.isoformat(),
        )

    def test_description_and_comment_intervals_are_both_kept(self):
        result = parse_sps_instructions(
            source_shift(notes="uni 8-10", comment="uni 13-14"), self.timezone
        )
        self.assertEqual((), result.issues)
        self.assertEqual(
            [NOTES_SOURCE_ID, "comment-1"], [i.source_id for i in result.intervals]
        )

    def test_same_interval_in_description_and_comment_is_planned_once(self):
        result = parse_sps_instructions(
            source_shift(notes="uni 8-10", comment="uni 8-10"), self.timezone
        )
        self.assertEqual(1, len(result.intervals))
        self.assertEqual(["duplicate_uni_interval"], [i.code for i in result.issues])

    def test_abbreviated_weekdays_resolve_like_full_names(self):
        for spelling in ("fre", "fre.", "FRE", "fredag"):
            with self.subTest(spelling=spelling):
                result = parse_sps_instructions(
                    source_shift(
                        starts_at="2026-09-17T13:00:00+02:00",
                        ends_at="2026-09-18T16:00:00+02:00",
                        comment=None,
                        notes=f"uni 12-14 {spelling}",
                    ),
                    self.timezone,
                )
                self.assertEqual((), result.issues)
                self.assertEqual(18, result.intervals[0].interval.starts_at.day)

    def test_friday_suffix_resolves_second_day_of_september_shift(self):
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-09-17T13:00:00+02:00",
                ends_at="2026-09-18T16:00:00+02:00",
                comment="uni 12-14 fredag",
            ),
            self.timezone,
        )
        self.assertEqual((), result.issues)
        self.assertEqual(
            "2026-09-18T12:00:00+02:00",
            result.intervals[0].interval.starts_at.isoformat(),
        )
        self.assertEqual(2, result.intervals[0].interval.hours)

    def test_weekday_suffix_applies_to_split_intervals(self):
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-09-17T13:00:00+02:00",
                ends_at="2026-09-18T16:00:00+02:00",
                comment="UNI 8-10 & 12-14 FREDAG",
            ),
            self.timezone,
        )
        self.assertEqual((), result.issues)
        self.assertEqual(2, len(result.intervals))
        self.assertTrue(all(i.interval.starts_at.day == 18 for i in result.intervals))

    def test_weekday_never_guesses_between_dates_or_overrides_bounds(self):
        for end, comment, code in (
            ("2026-09-25T16:00:00+02:00", "uni 12-14 fredag", "ambiguous_uni_weekday"),
            (
                "2026-09-18T16:00:00+02:00",
                "uni 12-14 lørdag",
                "uni_weekday_outside_shift",
            ),
            ("2026-09-18T13:00:00+02:00", "uni 12-14 fredag", "uni_outside_shift"),
            (
                "2026-09-18T16:00:00+02:00",
                "uni 2026-09-17 14-15 fredag",
                "conflicting_uni_date",
            ),
        ):
            with self.subTest(code=code):
                result = parse_sps_instructions(
                    source_shift(
                        starts_at="2026-09-17T13:00:00+02:00",
                        ends_at=end,
                        comment=comment,
                    ),
                    self.timezone,
                )
                self.assertEqual((), result.intervals)
                self.assertEqual(code, result.issues[0].code)

    def test_split_intervals_preserve_gap_and_hours(self) -> None:
        result = parse_sps_instructions(source_shift(), self.timezone)

        self.assertEqual([], list(result.issues))
        self.assertEqual(2, len(result.intervals))
        self.assertEqual(2, result.intervals[0].interval.hours)
        self.assertEqual(1, result.intervals[1].interval.hours)
        self.assertEqual(
            "2026-09-14T10:00:00+02:00",
            result.intervals[0].interval.ends_at.isoformat(),
        )
        self.assertEqual(
            "2026-09-14T13:00:00+02:00",
            result.intervals[1].interval.starts_at.isoformat(),
        )

    def test_undated_comment_on_overnight_shift_is_ambiguous(self) -> None:
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-09-14T22:00:00+02:00",
                ends_at="2026-09-15T07:00:00+02:00",
                comment="uni 23-1",
            ),
            self.timezone,
        )

        self.assertEqual([], list(result.intervals))
        self.assertEqual("ambiguous_uni_date", result.issues[0].code)

    def test_explicit_date_supports_overnight_interval(self) -> None:
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-09-14T22:00:00+02:00",
                ends_at="2026-09-15T07:00:00+02:00",
                comment="uni 2026-09-14 23-1",
            ),
            self.timezone,
        )

        self.assertEqual([], list(result.issues))
        self.assertEqual(
            "2026-09-15T01:00:00+02:00",
            result.intervals[0].interval.ends_at.isoformat(),
        )

    def test_nonexistent_dst_time_is_not_guessed(self) -> None:
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-03-29T00:00:00+01:00",
                ends_at="2026-03-29T05:00:00+02:00",
                comment="uni 2:30-4",
            ),
            self.timezone,
        )

        self.assertEqual([], list(result.intervals))
        self.assertEqual("nonexistent_local_time", result.issues[0].code)

    def test_ambiguous_dst_time_is_not_guessed(self) -> None:
        result = parse_sps_instructions(
            source_shift(
                starts_at="2026-10-25T00:00:00+02:00",
                ends_at="2026-10-25T05:00:00+01:00",
                comment="uni 2:30-4",
            ),
            self.timezone,
        )

        self.assertEqual([], list(result.intervals))
        self.assertEqual("ambiguous_local_time", result.issues[0].code)

    def test_duplicate_interval_is_reported_and_planned_once(self) -> None:
        shift = source_shift(comment="uni 8-10\nuni 8-10")
        result = parse_sps_instructions(shift, self.timezone)

        self.assertEqual(1, len(result.intervals))
        self.assertEqual("duplicate_uni_interval", result.issues[0].code)


if __name__ == "__main__":
    unittest.main()
