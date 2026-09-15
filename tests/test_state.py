from __future__ import annotations

import unittest

from helpers import moment, source_shift

from teamup_shift_sync.state import SyncState


class StateTests(unittest.TestCase):
    def test_source_comments_are_snapshotted_separately(self) -> None:
        shift = source_shift()
        with SyncState(":memory:") as state:
            state.record_source_snapshot(shift, moment("2026-09-20T20:00:00+02:00"))
            occurrence_count = state.connection.execute(
                "SELECT COUNT(*) FROM source_occurrences"
            ).fetchone()[0]
            comment_count = state.connection.execute(
                "SELECT COUNT(*) FROM source_comments"
            ).fetchone()[0]
            columns = {
                row["name"]
                for row in state.connection.execute(
                    "PRAGMA table_info(source_occurrences)"
                )
            }

        self.assertEqual(1, occurrence_count)
        self.assertEqual(1, comment_count)
        self.assertIn("recurrence_start", columns)
        self.assertIn("source_version", columns)


if __name__ == "__main__":
    unittest.main()
