"""Build and verify a portable desktop bundle for the current platform."""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ".exe" if os.name == "nt" else ""
PLATFORM = platform.system().lower()
ARCH = platform.machine().lower().replace("amd64", "x86_64")
BUNDLE = ROOT / "dist" / f"teamup-shift-sync-{PLATFORM}-{ARCH}"


def run(*command: str, env: dict[str, str] | None = None) -> None:
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def main() -> int:
    shutil.rmtree(BUNDLE, ignore_errors=True)
    BUNDLE.mkdir(parents=True)
    (ROOT / "build").mkdir(exist_ok=True)

    run(
        sys.executable,
        "-m",
        "PyInstaller",
        "--noconfirm",
        "--clean",
        "--onefile",
        "--name",
        "teamup-shift-sync-worker",
        "--paths",
        "src",
        "--collect-data",
        "teamup_shift_sync",
        "--collect-all",
        "tzdata",
        "--collect-all",
        "keyring",
        "--distpath",
        str(BUNDLE),
        "--workpath",
        str(ROOT / "build" / "pyinstaller"),
        "--specpath",
        str(ROOT / "build"),
        "scripts/worker_entry.py",
    )
    run("cargo", "build", "--release", "--manifest-path", "desktop/Cargo.toml")
    gui = ROOT / "desktop" / "target" / "release" / f"teamup-shift-sync-gui{EXE}"
    shutil.copy2(gui, BUNDLE / gui.name)
    shutil.copytree(ROOT / "fixtures", BUNDLE / "fixtures")
    shutil.copy2(ROOT / "config.example.toml", BUNDLE / "config.example.toml")

    with tempfile.TemporaryDirectory() as data_dir:
        env = os.environ.copy()
        env.update(
            {
                "TEAMUP_SHIFT_SYNC_CONFIG": str(
                    BUNDLE / "fixtures" / "offline-config.toml"
                ),
                "TEAMUP_FIXTURE": str(BUNDLE / "fixtures" / "representative-week.json"),
                "TEAMUP_SHIFT_SYNC_DATA_DIR": data_dir,
                "PYTHONTZPATH": "",
            }
        )
        run(str(BUNDLE / gui.name), "--self-check", env=env)

    archive = shutil.make_archive(str(BUNDLE), "zip", BUNDLE.parent, BUNDLE.name)
    print(f"Created {archive}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
