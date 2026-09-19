import io
import json
import os
import tempfile
import unittest
from contextlib import redirect_stderr
from copy import deepcopy
from datetime import date
from pathlib import Path
from typing import ClassVar
from unittest.mock import patch
from zoneinfo import ZoneInfo

from teamup_shift_sync.destinations import employment_available
from teamup_shift_sync.setup import Setup, SetupError, calendar_reference
from teamup_shift_sync.teamup import TeamUpClient, TeamUpError
from teamup_shift_sync.worker import process_line


class Vault:
    def __init__(self):
        self.items = {}

    def get(self, key):
        return self.items[key]

    def put(self, key, value):
        self.items[key] = value


class Calendar:
    calendars: ClassVar = [
        {"id": 1, "name": "Helper One"},
        {"id": 2, "name": "Birthdays"},
    ]
    failure = False

    def __init__(self, config):
        self.timezone = ZoneInfo(config.timezone)

    def _get(self, *args):
        if self.failure:
            raise TeamUpError("restricted")
        return {"configuration": {"subcalendars": deepcopy(self.calendars)}}

    def fetch_occurrences(self, *args):
        return ()


class Destination:
    def __init__(self):
        self.people = [{"vid": 10, "navn": "Helper One"}]
        self.customers = [{"id": 20, "navn": "Customer"}]
        self.grants = [{"id": 30, "navn": "Grant", "valgbar": True}]
        self.portfolios = [
            {"id": 40, "name": "SPS practical help", "type": "Ordnings SPS"}
        ]
        self.employments = [
            {"helperId": 50, "helperName": "Helper One", "isActive": True}
        ]
        self.requests = []

    def request(self, system, action, payload):
        self.requests.append((system, action, payload))
        return deepcopy(
            {
                "hjaelperliste": {"grupper": [{"hjaelpere": self.people}]},
                "muligheder": {
                    "muligheder": {"kunder": self.customers, "bevillinger": self.grants}
                },
                "portfolios": self.portfolios,
                "types": [{"id": 0, "name": "Ordinary hours"}],
                "employments": self.employments,
            }[action]
        )


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.vault = Vault()
        self.destination = Destination()
        self.setup = self.restart()

    def restart(self):
        return Setup(
            self.root, self.destination, vault=self.vault, client_factory=Calendar
        )

    def connected(self):
        self.setup.connect(
            "https://teamup.com/ksTestSecret?view=w", "test-private-token"
        )
        self.setup.discover()

    def configured(self):
        self.connected()
        self.setup.edit({"source": "2", "excluded": True})

    def test_employment_dates_and_missing_status_do_not_assume_active(self):
        today = date(2026, 9, 19)
        self.assertFalse(employment_available({}, today))
        self.assertFalse(
            employment_available(
                {"isActive": False, "startDate": "2026-01-01", "endDate": "2026-09-18"},
                today,
            )
        )
        self.assertTrue(
            employment_available(
                {"isActive": False, "startDate": "2026-01-01", "endDate": "2026-09-19"},
                today,
            )
        )
        self.assertFalse(
            employment_available({"isActive": False, "startDate": "2026-09-20"}, today)
        )

    def test_links_never_become_arbitrary_network_targets(self):
        for link in (
            "http://teamup.com/ksabc",
            "https://evil.test/ksabc",
            "https://teamup.com.evil/ksabc",
            "https://user@teamup.com/ksabc",
            "https://teamup.com/123",
            "ksabc",
        ):
            with self.subTest(link=link), self.assertRaises(SetupError):
                calendar_reference(link)
        self.assertEqual(
            "ksabc",
            calendar_reference("https://teamup.com/ksabc/events/123?date=2026-09-19"),
        )

    def test_resume_and_explicit_confirmation_without_plaintext_secrets(self):
        self.configured()
        restarted = self.restart()
        self.assertEqual("helpers", restarted.data["stage"])
        self.assertEqual("10", restarted.data["mappings"][0]["mithf"])
        self.assertTrue(restarted.data["mappings"][1]["excluded"])
        self.assertEqual("0", restarted.data["registration_type"])
        restarted.confirm()
        config = self.restart().config()
        self.assertEqual("10", config.helpers["1"].mithf_id)
        self.assertEqual("50", config.helpers["1"].duos_employee_number)
        self.assertEqual("20", config.mithf_customer_id)
        self.assertEqual({"1"}, set(config.helpers))
        public = self.setup.path.read_text() + json.dumps(restarted.view())
        self.assertNotIn("ksTestSecret", public)
        self.assertNotIn("test-private-token", public)
        if os.name != "nt":
            self.assertEqual(0o600, self.setup.path.stat().st_mode & 0o777)

    def test_resume_preserves_danish_names_with_windows_default_encoding(self):
        self.destination.customers[0]["navn"] = "Søren Ærø"
        self.configured()
        original_read = Path.read_text

        def windows_read(path, *args, **kwargs):
            kwargs.setdefault("encoding", "cp1252")
            return original_read(path, *args, **kwargs)

        with patch.object(Path, "read_text", windows_read):
            restarted = self.restart()
            self.assertEqual(self.setup.data["account"], restarted.data["account"])
            restarted.confirm()
            self.assertEqual("ready", self.restart().data["stage"])

    def test_failed_calendar_access_retains_retryable_credentials(self):
        with patch.object(Calendar, "failure", True), self.assertRaises(TeamUpError):
            self.connected()
        resumed = self.restart()
        self.assertTrue(resumed.view()["has_credentials"])
        self.assertEqual("source", resumed.data["stage"])
        resumed.refresh_source()
        self.assertEqual("destinations", resumed.data["stage"])

    def test_ambiguous_names_require_selection_and_mithf_duplicates_block(self):
        self.destination.people.append({"vid": 11, "navn": "Helper One"})
        self.configured()
        self.assertEqual("", self.setup.data["mappings"][0]["mithf"])
        self.setup.edit({"source": "1", "mithf": "11"})
        with self.assertRaisesRegex(SetupError, "samme navn"):
            self.setup.confirm()
        self.assertNotEqual("ready", self.restart().data["stage"])

    def test_duos_duplicates_are_not_silently_selected(self):
        self.destination.employments.append(
            {"helperId": 51, "helperName": "Helper One", "isActive": True}
        )
        self.configured()
        self.assertEqual("", self.setup.data["mappings"][0]["duos"])
        with self.assertRaises(SetupError):
            self.setup.confirm()
        self.setup.edit({"source": "1", "duos": "51"})
        self.setup.confirm()
        self.assertEqual("51", self.setup.config().helpers["1"].duos_employee_number)

    def test_no_sps_or_active_employment_blocks(self):
        self.setup.connect("https://teamup.com/ksTestSecret", "private")
        self.destination.portfolios[0]["type"] = "Other"
        with self.assertRaisesRegex(SetupError, "ingen aktiv SPS"):
            self.setup.discover()
        self.destination.portfolios[0]["type"] = "Ordnings SPS"
        self.destination.employments[0]["isActive"] = False
        with self.assertRaisesRegex(SetupError, "aktive hjælpere"):
            self.setup.discover()

    def test_multiple_mithf_options_block_without_first_choice(self):
        self.setup.connect("https://teamup.com/ksTestSecret", "private")
        self.destination.grants.append({"id": 31, "navn": "Another", "valgbar": True})
        with self.assertRaisesRegex(SetupError, "flere kunder/bevillinger"):
            self.setup.discover()
        self.assertEqual("", self.setup.data["account"])

    def test_multiple_sps_arrangements_need_explicit_selection(self):
        self.destination.portfolios.append(
            {"id": 41, "name": "SPS secretary", "type": "Ordnings SPS"}
        )
        self.connected()
        self.assertEqual("", self.setup.data["arrangement"])
        self.assertFalse(
            any(action == "employments" for _, action, _ in self.destination.requests)
        )
        self.setup.discover("41")
        self.assertEqual("41", self.setup.data["arrangement"])
        employment_reads = [
            payload
            for _, action, payload in self.destination.requests
            if action == "employments"
        ]
        self.assertEqual("41", employment_reads[-1]["portfolioId"])
        self.assertIn("dateOfActiveEmployment", employment_reads[-1])

    def test_changed_identity_invalidates_confirmation_before_preview(self):
        self.configured()
        self.setup.confirm()
        self.destination.people[0]["vid"] = 99
        with self.assertRaisesRegex(SetupError, "ændret"):
            self.setup.preview(self.root / "sync.sqlite3", None, None)
        resumed = self.restart()
        self.assertEqual("destinations", resumed.data["stage"])
        self.assertFalse(resumed.data["mappings"])

    def test_new_calendar_on_retry_requires_an_explicit_mapping_or_exclusion(self):
        self.configured()
        with patch.object(
            Calendar,
            "calendars",
            [*Calendar.calendars, {"id": 3, "name": "Another helper"}],
        ):
            self.setup.refresh_source()
            self.setup.discover()
            self.assertEqual(3, len(self.setup.data["mappings"]))
            with self.assertRaises(SetupError):
                self.setup.confirm()

    def test_changed_single_account_invalidates_confirmation(self):
        self.configured()
        self.setup.confirm()
        self.destination.customers[0]["id"] = 99
        with self.assertRaisesRegex(SetupError, "ændret"):
            self.setup.revalidate()

    def test_import_is_a_review_and_does_not_change_original(self):
        path = self.root / "old.toml"
        path.write_text("""[teamup]
calendar_key = "ksTestSecret"
api_key = "private-imported-key"
helper_field = "subcalendar"
[teamup.helper_subcalendars]
"1" = "Helper One"
[duos]
arrangement_id = "40"
registration_type = "0"
[[helpers]]
teamup_key = "1"
teamup_display_name = "Helper One"
mithf_name = "Helper One"
duos_name = "Helper One"
duos_employee_number = "50"
""")
        before = path.read_bytes()
        self.setup.import_config(path)
        self.setup.discover()
        self.assertEqual("helpers", self.setup.data["stage"])
        self.assertEqual(before, path.read_bytes())
        self.assertNotIn("private-imported-key", self.setup.path.read_text())
        self.assertIn("importeret", self.setup.data["notice"])

    def test_worker_never_echoes_private_exception(self):
        with patch("teamup_shift_sync.worker.Setup") as setup:
            setup.return_value.connect.side_effect = RuntimeError(
                "SECRET_URL_AND_NAMES"
            )
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                response = process_line(
                    json.dumps(
                        {
                            "protocol": 1,
                            "id": "1",
                            "method": "setup_connect",
                            "params": {"link": "private", "api_key": "private"},
                        }
                    )
                )
        self.assertNotIn("SECRET_URL_AND_NAMES", response + stderr.getvalue())
        self.assertEqual("setup_error", json.loads(response)["payload"]["code"])

    def test_reserved_time_blocks_do_not_pass_as_readable_shifts(self):
        from types import SimpleNamespace

        event = SimpleNamespace(title="Reserved", raw={"readonly": True})
        with (
            patch.object(Calendar, "fetch_occurrences", return_value=(event,)),
            self.assertRaisesRegex(SetupError, "skjuler vagtdetaljer"),
        ):
            self.connected()
        self.assertEqual("source", self.restart().data["stage"])

    def test_comment_visibility_is_checked_by_real_client(self):
        class Restricted:
            def get_json(self, url, **kwargs):
                if url.endswith("configuration"):
                    return {"configuration": {"subcalendars": Calendar.calendars}}
                if url.endswith("/events"):
                    return {"events": [{"id": "event"}]}
                return {
                    "event": {
                        "id": "event",
                        "comments_enabled": True,
                        "comments_visibility": "users_with_modify_permission",
                        "readonly": True,
                    }
                }

        setup = Setup(
            self.root,
            self.destination,
            vault=self.vault,
            client_factory=lambda cfg: TeamUpClient(cfg, Restricted()),
        )
        with self.assertRaisesRegex(TeamUpError, "hidden"):
            setup.connect("https://teamup.com/ksTestSecret", "private")
        self.assertEqual("source", self.restart().data["stage"])
