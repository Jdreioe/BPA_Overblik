import unittest
from dataclasses import replace

from helpers import config

from teamup_shift_sync.config import configuration_issues


class ConfigTests(unittest.TestCase):
    def test_missing_destination_mappings_are_reported_without_credentials(self):
        cfg = replace(
            config(),
            teamup_helper_field="subcalendar",
            teamup_subcalendar_helpers={"42": "Helper"},
            helpers={},
        )
        issues = configuration_issues(cfg)
        self.assertIn("1 source helpers lack destination mappings", issues)
        self.assertFalse(any(cfg.teamup_api_key in issue for issue in issues))

    def test_ordinary_type_zero_is_valid(self):
        self.assertEqual(
            (), configuration_issues(replace(config(), duos_registration_type="0"))
        )
