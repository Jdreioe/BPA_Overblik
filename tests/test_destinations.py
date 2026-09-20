import unittest
from dataclasses import replace

from helpers import config, moment

from teamup_shift_sync.browser import DestinationError
from teamup_shift_sync.destinations import Destinations
from teamup_shift_sync.models import DestinationSnapshot, Outcome, PlanItem


class Responses:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.requests = []

    def request(self, system, action, payload):
        self.requests.append((system, action, payload))
        return self.responses.pop(0)


def registration(id_):
    return {
        "id": id_,
        "portfolioId": 35505,
        "helperId": 123,
        "dutyTypeId": 0,
        "startDate": "2026-09-14T08:00:00+02:00",
        "endDate": "2026-09-14T10:00:00+02:00",
        "statusId": 0,
    }


class DestinationTests(unittest.TestCase):
    start = moment("2026-09-14T00:00:00+02:00")
    end = moment("2026-09-21T00:00:00+02:00")

    def test_duos_reads_all_pages_and_preserves_ordinary_type_zero(self):
        transport = Responses(
            {"data": [registration(1)], "hasMore": True},
            {"data": [registration(2)], "hasMore": False},
        )
        rows = Destinations(transport, config()).read_duos(self.start, self.end)
        self.assertEqual(["1", "2"], [r.id for r in rows])
        self.assertEqual("0", rows[0].registration_type)
        self.assertEqual(1, transport.requests[1][2]["skip"])

    def test_repeated_page_cannot_be_treated_as_complete(self):
        transport = Responses(
            {"data": [registration(1)], "hasMore": True},
            {"data": [registration(1)], "hasMore": False},
        )
        with self.assertRaisesRegex(DestinationError, "repeated"):
            Destinations(transport, config()).read_duos(self.start, self.end)

    def test_carry_in_uses_actual_start_and_reads_sps_details(self):
        raw = {
            "id": "shift",
            "dato": "2026-09-14",
            "startFaktisk": "2026-09-13",
            "start": "20:00",
            "slutdato": "2026-09-15",
            "slut": "07:30",
            "daekket": True,
            "navn": "Mit Helper",
        }
        transport = Responses(
            {"fra": "2026-09-07", "til": "2026-09-21", "dage": {"2026-09-14": [raw]}},
            {
                "ekstra": {
                    "shift": {
                        "paa": [
                            {
                                "id": "sps",
                                "navn": "SPS timer",
                                "fraDato": "14.09.2026",
                                "fra": "08:00",
                                "tilDato": "14.09.2026",
                                "til": "10:00",
                            }
                        ]
                    }
                }
            },
        )
        rows = Destinations(transport, config()).read_mithf(self.start, self.end)
        self.assertEqual(moment("2026-09-13T20:00:00+02:00"), rows[0].starts_at)
        self.assertEqual(2, rows[0].sps_intervals[0].hours)
        self.assertEqual(("sps",), rows[0].sps_record_ids)

    def test_unconfirmed_range_is_an_error_even_when_empty(self):
        transport = Responses({"fra": "2026-09-14", "til": "2026-09-21", "dage": {}})
        with self.assertRaisesRegex(DestinationError, "coverage"):
            Destinations(transport, config()).read_mithf(self.start, self.end)

    def validating_transport(self):
        return Responses(
            {"grupper": [{"hjaelpere": [{"navn": "Mit Helper", "vid": 7}]}]},
            {"muligheder": {"kunder": [{"id": 1}], "bevillinger": [{"id": 2, "valgbar": True}]}},
            [{"id": "arrangement", "type": "Ordnings SPS"}],
            [{"id": "Almindelig"}],
            # Employed as of today, but not on the past Monday below.
            [{"helperId": "123", "helperName": "DUOS Helper", "startDate": "2026-09-10"}],
        )

    def test_validate_resolves_identities_as_of_today(self):
        transport = self.validating_transport()
        Destinations(transport, config()).validate(moment("2026-09-19T12:00:00+02:00"))
        duos_reads = [payload for _, action, payload in transport.requests if action in {"portfolios", "employments"}]
        self.assertEqual(
            ["2026-09-19", "2026-09-19"],
            [
                duos_reads[0]["dateOfActivePortfolio"],
                duos_reads[1]["dateOfActiveEmployment"],
            ],
        )

    def test_validate_against_a_past_monday_rejects_current_employment(self):
        # Documents why callers must pass today: a helper hired after the
        # selected week is valid now but was not employed then.
        transport = self.validating_transport()
        with self.assertRaisesRegex(DestinationError, "employment"):
            Destinations(transport, config()).validate(moment("2026-08-31T00:00:00+02:00"))

    def test_duos_write_sends_offset_and_does_not_accept_registration(self):
        transport = Responses(None)
        cfg = replace(config(), duos_arrangement_id="35505", duos_registration_type="0")
        payload = {
            "arrangement_id": "35505",
            "employee_number": "123",
            "registration_type": "0",
            "starts_at": "2025-09-14T08:00:00+02:00",
            "ends_at": "2025-09-14T10:00:00+02:00",
        }
        item = PlanItem(
            "source", "duos", "duos.interval:1", Outcome.WOULD_CREATE, "", payload
        )
        Destinations(transport, cfg).write(item, DestinationSnapshot())
        self.assertEqual(["register"], [r[1] for r in transport.requests])
        sent = transport.requests[0][2]
        self.assertEqual(0, sent["typeId"])
        self.assertEqual(120, sent["clientTimeSnapshot"]["clientStartOffsetMinutes"])
        self.assertEqual(120, sent["clientTimeSnapshot"]["visibleDurationMinutes"])
