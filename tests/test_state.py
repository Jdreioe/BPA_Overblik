from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from helpers import moment, source_shift

from teamup_shift_sync.state import SyncState


class StateTests(unittest.TestCase):
    def test_apply_lock_blocks_other_process_and_releases_after_error(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "state.sqlite3"
            command = [
                sys.executable,
                "-c",
                """
import sys
from teamup_shift_sync.state import SyncState
with SyncState(sys.argv[1]) as state:
    try:
        with state.exclusive_apply():
            pass
    except ValueError:
        sys.exit(23)
""",
                str(path),
            ]
            with SyncState(path) as state:
                with (
                    self.assertRaisesRegex(RuntimeError, "interrupted"),
                    state.exclusive_apply(),
                ):
                    blocked = subprocess.run(
                        command, capture_output=True, timeout=10, check=False
                    )
                    self.assertEqual(blocked.returncode, 23, blocked.stderr)
                    raise RuntimeError("interrupted")
                released = subprocess.run(
                    command, capture_output=True, timeout=10, check=False
                )
                self.assertEqual(released.returncode, 0, released.stderr)

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
