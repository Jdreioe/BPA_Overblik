import unittest

from helpers import moment

from teamup_shift_sync.source_rules import is_reminder, meeting_item


class SourceRulesTests(unittest.TestCase):
    def test_title_rules(self):
        self.assertTrue(is_reminder(" Husk at checke vagtplanen på AXP "))
        self.assertFalse(is_reminder("Husk at checker"))
        start = moment("2026-09-14T13:00:00+02:00")
        end = moment("2026-09-14T14:00:00+02:00")
        item = meeting_item(" p-møde ", "event", start, end)
        self.assertIsNotNone(item)
        self.assertEqual("Vagtmøde", item.payload["category"])
        self.assertEqual(start.isoformat(), item.payload["starts_at"])
        self.assertEqual(end.isoformat(), item.payload["ends_at"])
        self.assertEqual("mithf", item.system)
        self.assertIsNone(meeting_item("Almindelig vagt", "event", start, end))
