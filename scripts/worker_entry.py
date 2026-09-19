"""PyInstaller entry point for the app-owned worker."""

from teamup_shift_sync.worker import main

if __name__ == "__main__":
    raise SystemExit(main())
