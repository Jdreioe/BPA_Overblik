"""The worker protocol is UTF-8 JSON even under a hostile locale.

A frozen Windows worker ignores PYTHONUTF8 and falls back to the ANSI
code page, which encodes Danish dashes and arrows as non-UTF-8 bytes
that the desktop shell must reject. The worker therefore forces UTF-8
on its own stdio; this spawns it under LC_ALL=C with UTF-8 mode off
(which reproduces the failure on any platform) and asserts a fixture
preview still parses with its Danish text intact.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class WorkerStdioTests(unittest.TestCase):
    def test_fixture_preview_survives_c_locale_without_utf8_mode(self):
        with tempfile.TemporaryDirectory() as data_dir:
            env = dict(os.environ)
            env.update(
                {
                    "LC_ALL": "C",
                    "LANG": "C",
                    "PYTHONUTF8": "0",
                    "PYTHONTZPATH": "",
                }
            )
            # An explicitly set encoding would mask the bug; the worker
            # must guarantee UTF-8 itself.
            env.pop("PYTHONIOENCODING", None)
            frames = "\n".join(
                json.dumps(
                    {"protocol": 1, "id": request_id, "method": method, "params": params}
                )
                for request_id, method, params in (
                    ("1", "ping", {}),
                    (
                        "2",
                        "preview_fixture",
                        {
                            "config_path": str(
                                ROOT / "fixtures" / "offline-config.toml"
                            ),
                            "fixture_path": str(
                                ROOT / "fixtures" / "representative-week.json"
                            ),
                            "state_path": str(Path(data_dir) / "sync.sqlite3"),
                            "from": "2026-09-14",
                            "to": "2026-09-20",
                        },
                    ),
                )
            )
            proc = subprocess.Popen(
                [sys.executable, "-m", "teamup_shift_sync.worker"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                # Our side always reads UTF-8: any other byte from the
                # worker is the bug, and strict decoding fails loudly.
                encoding="utf-8",
                errors="strict",
                env=env,
                cwd=ROOT,
            )
            try:
                out, err = proc.communicate(frames + "\n", timeout=180)
            except subprocess.TimeoutExpired:
                proc.kill()
                _, err = proc.communicate()
                self.fail(f"worker hung under C locale; stderr tail:\n{err[-2000:]}")
            lines = out.splitlines()
            self.assertEqual(len(lines), 2, f"expected two frames, stderr:\n{err}")
            ping = json.loads(lines[0])
            self.assertEqual(ping["event"], "result")
            preview = json.loads(lines[1])
            self.assertEqual(
                preview["event"], "result", f"worker error: {preview['payload']}"
            )
            # The en dash in time labels must arrive intact, not as mojibake.
            self.assertIn(
                "–", json.dumps(preview["payload"], ensure_ascii=False)
            )
            self.assertIn("worker stage: preview_fixture done", err)
