# Rust CLI

Build from the repository root:

```sh
cargo build -p teamup-shift-sync-cli
```

The binary is `target/debug/teamup-shift-sync-rust`, with `.exe` on Windows.
It runs without Python, as do the desktop app and all tests.

## Offline preview

```sh
cargo run -p teamup-shift-sync-cli -- dry-run \
  --config fixtures/offline-config.toml \
  --fixture fixtures/representative-week.json \
  --state .local/offline-rust.sqlite3 \
  --from 2026-09-14 --to 2026-09-20 \
  --now 2026-09-20T20:00:00+02:00
```

Add `--json` for a JSON object containing `mode`, `plan`, `digest` and
`has_blockers`. Reports include source identities and transfer values so the
batch can be reviewed. Treat reports as private account data.

Dates are inclusive and use the configured timezone. Without dates, previews
select the current local Monday-to-Sunday week. With `--from` alone they select
seven days from that date. The representative fixture has blocked items and
therefore exits with code 1. A preview can initialize its database schema but
does not record successful transfers or contact destinations.

## Live preview and apply

Complete setup in the desktop app first. Use its data directory below.
The app uses `TEAMUP_SHIFT_SYNC_DATA_DIR` when set; otherwise it uses
`$XDG_DATA_HOME/teamup-shift-sync` on Linux, falling back to
`~/.local/share/teamup-shift-sync`, `~/Library/Application Support/teamup-shift-sync`
on macOS, or `%APPDATA%/teamup-shift-sync` on Windows. It must contain `setup.json`;
TeamUp credentials stay in the OS vault. Live commands derive the database path
from the saved account choices and mappings.

```sh
cargo run -p teamup-shift-sync-cli -- login --data-dir /path/to/app-data
```

Log in through both opened browser windows, then press Enter in the terminal.
The command checks read access and closes its browsers. `login` is the only
command that opens a window; every other command runs Chromium headless
against these separate Rust profiles, which preserve the login. MitHF's shift
calendar is entered through the site's own "Åbn din vagtplan" action, so it
does not have to be opened by hand.
If Chromium cannot be found, set `TEAMUP_BROWSER_PATH` to its executable.

```sh
cargo run -p teamup-shift-sync-cli -- capture \
  --data-dir /path/to/app-data --out docs/service-shapes.json
```

`capture` issues every MitHF and DUOS read the native adapters depend on and
records the JSON structure it gets back: object keys and leaf types only, never
a value. Strings are reduced to a form tag (`text`, `digits`, `date-iso`,
`date-dk`, `time`, `datetime-iso`, `empty`), and object keys that are data
themselves — MitHF keys days by date and extras by shift id — become tags too.
The result carries no shift, helper or account content and is meant to be
committed as the recorded wire contract.

Use it to check what the readers assume against what the services send. The
native readers had no coverage of real service JSON, and two fields typed as
JSON booleans in Rust (`valgbar`, `daekket`) are plain truthy values in the
previous Python implementation. A shape file makes that class of mismatch
visible instead of leaving it to fail on a live account.

TeamUp is not captured: its reads tolerate missing and differently typed fields
already, and they use no browser session. `ekstra` is only reached when the
selected week reports a shift; anything skipped is listed under `missing`.

```sh
cargo run -p teamup-shift-sync-cli -- dry-run --live \
  --data-dir /path/to/app-data --from 2026-09-14 --to 2026-09-20
```

Review every item. Resolve blocked items and preview again before applying.
Copy the exact `Plan digest` from the live preview into `--approve`:

```sh
cargo run -p teamup-shift-sync-cli -- apply \
  --data-dir /path/to/app-data --from 2026-09-14 --to 2026-09-20 \
  --approve YOUR_REVIEWED_64_CHARACTER_DIGEST
```

`apply` makes real MitHF and DUOS changes. It rereads TeamUp and destinations,
rejects a changed approval, and verifies every submitted step through read-back.
If it fails after submission, keep the database: the uncertain records support
recovery. Run a fresh preview before deciding what to do next.

## Forget local records

For a destination deletion that you deliberately want to recreate, use its exact
source key from the preview and the exact account database under that data
directory. If several `sync-*.sqlite3` files exist, establish which belongs to
the saved setup before forgetting anything:

```sh
cargo run -p teamup-shift-sync-cli -- forget \
  --state /path/to/sync-ACCOUNT.sqlite3 --shift 'CALENDAR:EVENT:OCCURRENCE'
```

This clears only that source's stored steps and holds the same apply lock as
transfers. It never changes a destination. Review a new live preview afterwards.

Exit codes: 0 means success, 1 means a blocked preview or nothing to forget,
and 2 means invalid arguments or an operational error. Live CLI behavior has
not yet been tested against real accounts.
