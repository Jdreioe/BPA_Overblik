from __future__ import annotations

import os
from pathlib import Path

_SUPPORTED_KEYS = {"TEAMUP_API", "TEAMUP_CALENDAR_KEY", "TEAMUP_BEARER_TOKEN"}


def load_local_environment(config_path: Path) -> None:
    """Load supported secrets from project or parent .env without overriding the process."""
    config_dir = config_path.resolve().parent
    for path in (config_dir.parent / ".env", config_dir / ".env"):
        if not path.is_file():
            continue
        for line in path.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if stripped.startswith("export "):
                stripped = stripped.removeprefix("export ").lstrip()
            key, separator, value = stripped.partition("=")
            key = key.strip()
            if not separator or key not in _SUPPORTED_KEYS:
                continue
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
                value = value[1:-1]
            os.environ.setdefault(key, value)
