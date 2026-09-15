from __future__ import annotations

import unittest
from dataclasses import replace
from datetime import date
from typing import Any
from zoneinfo import ZoneInfo

from helpers import config

from teamup_shift_sync.teamup import TeamUpClient, TeamUpError, _teamup_datetime


class FakeTransport:
    def __init__(self, responses: list[dict[str, Any]]):
        self.responses = responses
        self.calls: list[tuple[str, dict[str, str], dict[str, str]]] = []

    def get_json(self, url, *, headers, params):
        self.calls.append((url, dict(headers), dict(params)))
        return self.responses.pop(0)


def detail(*, comments=None, comments_enabled=True, event_id="123-rid-1789371000"):
    return {
        "event": {
            "id": event_id,
            "series_id": 123,
            "title": "Fixture shift",
            "who": "helper",
            "start_dt": "2026-09-14T07:30:00+02:00",
            "end_dt": "2026-09-14T15:00:00+02:00",
            "all_day": False,
            "readonly": False,
            "comments_enabled": comments_enabled,
            "comments": comments,
        }
    }


class TeamUpClientTests(unittest.TestCase):
    def test_week_read_includes_carry_in_and_filters_lookback_events(self):
        carry_in = detail(comments=[], event_id="carry")["event"]
        carry_in.update(
            start_dt="2026-09-13T20:00:00+02:00", end_dt="2026-09-14T08:00:00+02:00"
        )
        before = dict(carry_in, id="before", end_dt="2026-09-14T00:00:00+02:00")
        after = dict(
            carry_in,
            id="after",
            start_dt="2026-09-21T00:00:00+02:00",
            end_dt="2026-09-21T08:00:00+02:00",
        )
        transport = FakeTransport(
            [
                {"events": [{"id": item["id"]} for item in (carry_in, before, after)]},
                *[{"event": item} for item in (carry_in, before, after)],
            ]
        )
        occurrences = TeamUpClient(config(), transport).fetch_occurrences(
            date(2026, 9, 14), date(2026, 9, 20)
        )
        self.assertEqual(["carry"], [item.id for item in occurrences])
        self.assertEqual("2026-09-07", transport.calls[0][2]["startDate"])

    def test_subcalendar_identity_ignores_title_and_unrelated_calendar(self) -> None:
        cfg = replace(config(), teamup_subcalendar_helpers={"42": "Helper"})
        client = TeamUpClient(cfg, FakeTransport([]))
        raw = detail(comments=[])["event"]
        raw["subcalendar_ids"] = [42, 99]
        shift = client.to_source_shift(client._occurrence(raw), "subcalendar")
        self.assertEqual("42", shift.helper_key)

    def test_missing_or_multiple_helper_calendars_require_review(self) -> None:
        cfg = replace(
            config(), teamup_subcalendar_helpers={"42": "First", "43": "Second"}
        )
        client = TeamUpClient(cfg, FakeTransport([]))
        for ids in ([99], [42, 43]):
            with self.subTest(ids=ids):
                raw = detail(comments=[])["event"]
                raw["subcalendar_ids"] = ids
                with self.assertRaisesRegex(TeamUpError, "exactly one"):
                    client.to_source_shift(client._occurrence(raw), "subcalendar")

    def test_date_range_hydrates_comments_from_individual_event(self) -> None:
        transport = FakeTransport(
            [
                {"events": [{"id": "123-rid-1789371000"}], "timestamp": 1789410000},
                detail(
                    comments=[
                        {
                            "id": 91,
                            "event_id": "123-rid-1789371000",
                            "message": {
                                "markdown": "uni 8-10 & 13-14",
                                "html": "<p>uni</p>",
                            },
                            "creation_dt": "2026-09-14T15:10:00+02:00",
                            "update_dt": None,
                        }
                    ]
                ),
            ]
        )
        client = TeamUpClient(config(), transport)
        occurrences = client.fetch_occurrences(date(2026, 9, 14), date(2026, 9, 20))

        self.assertEqual(1, len(occurrences))
        self.assertEqual("123", occurrences[0].series_id)
        self.assertEqual("123-rid-1789371000", occurrences[0].id)
        self.assertEqual("uni 8-10 & 13-14", occurrences[0].comments[0].text)
        self.assertEqual("markdown", transport.calls[0][2]["format"])
        self.assertIn("Teamup-Token", transport.calls[0][1])

    def test_recurrence_origin_and_version_are_preserved(self) -> None:
        client = TeamUpClient(config(), FakeTransport([]))
        raw = detail(comments=[])["event"]
        raw["ristart_dt"] = "2026-09-14T05:30:00Z"
        raw["version"] = "abc123"

        shift = client.to_source_shift(client._occurrence(raw), "who")

        self.assertEqual(
            "2026-09-14T05:30:00+00:00", shift.recurrence_start.isoformat()
        )
        self.assertEqual("abc123", shift.source_version)

    def test_modify_only_comments_on_readonly_event_fail_closed(self) -> None:
        client = TeamUpClient(config(), FakeTransport([]))
        raw = detail(comments=[])["event"]
        raw["readonly"] = True
        raw["comments_visibility"] = "users_with_modify_permission"

        with self.assertRaisesRegex(TeamUpError, "may be hidden"):
            client._occurrence(raw)

    def test_comments_enabled_but_unreadable_fails_closed(self) -> None:
        transport = FakeTransport(
            [{"events": [{"id": "1"}]}, detail(comments=None, event_id="1")]
        )
        client = TeamUpClient(config(), transport)

        with self.assertRaisesRegex(TeamUpError, "not readable"):
            client.fetch_occurrences(date(2026, 9, 14), date(2026, 9, 20))

    def test_explicit_helper_field_becomes_mapping_key(self) -> None:
        client = TeamUpClient(config(), FakeTransport([]))
        occurrence = client._occurrence(detail(comments=[], event_id="1")["event"])

        shift = client.to_source_shift(occurrence, "who")

        self.assertEqual("helper", shift.helper_key)
        self.assertEqual("1", shift.occurrence_id)
        self.assertEqual("123", shift.event_id)

    def test_helper_field_is_never_guessed(self) -> None:
        client = TeamUpClient(config(), FakeTransport([]))
        occurrence = client._occurrence(detail(comments=[], event_id="1")["event"])

        with self.assertRaisesRegex(TeamUpError, "explicitly set"):
            client.to_source_shift(occurrence, "")

    def test_naive_ambiguous_api_datetime_is_not_guessed(self) -> None:
        with self.assertRaisesRegex(TeamUpError, "ambiguous local datetime"):
            _teamup_datetime("2026-10-25T02:30:00", ZoneInfo("Europe/Copenhagen"))


if __name__ == "__main__":
    unittest.main()
