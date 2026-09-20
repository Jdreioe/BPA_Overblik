# TeamUp shift sync

A local, manually triggered weekly importer for planning TeamUp helper shifts in
MitHF and registering completed SPS intervals in DUOS.

The first runnable milestone is deliberately offline. It implements
configuration, typed source/destination models, deterministic `uni` parsing,
reconciliation, resumable SQLite state, and a dry-run report. It cannot make
live MitHF or DUOS changes yet.

A read-only TeamUp API client and access probe are included. Event, description,
and comment access were verified against the configured calendar on 2026-09-15.
A real `uni` instruction in an event description was parsed end to end on that
date, including its Danish weekday suffix.

## Set up

Python 3.12 or newer is required.

```bash
cd /home/jdreioe/Projects/Vagtplanlaegning/teamup-shift-sync
python3 -m venv .venv
.venv/bin/python -m pip install --upgrade pip
.venv/bin/python -m pip install -e '.[dev]'
.venv/bin/teamup-shift-sync init
```

`init` refuses to overwrite `config.toml`. Both that file and `.local/` are
ignored by Git. Do not commit API keys or browser session data.

## Run the representative dry-run

This fixture demonstrates an existing MitHF shift, one existing DUOS interval,
one new DUOS interval, an overnight shift, and an ambiguous undated SPS comment
on a multi-day shift.

```bash
.venv/bin/teamup-shift-sync dry-run \
  --config fixtures/offline-config.toml \
  --fixture fixtures/representative-week.json \
  --state .local/offline.sqlite3 \
  --from 2026-09-14 \
  --to 2026-09-20 \
  --now 2026-09-20T20:00:00+02:00
```

Without `--from` and `--to`, the preview proposes the current local
Monday-through-Sunday week. The end of the displayed range is exclusive
internally, which lets shifts spanning midnight be selected correctly.

A dry-run reads the destination snapshot in its fixture and existing SQLite
sync records. It only creates the SQLite schema if needed; it never records a
successful step and never writes to external services.

## Re-synchronize a shift deleted by hand

If a MitHF shift or DUOS registration that was previously synchronized is
deleted directly in the destination, the plan reports it as `conflicted` and
will not recreate it. That guard is deliberate: a deletion is usually a decision
someone made on purpose, and the tool never silently undoes it.

When the deletion was a mistake and TeamUp should win, forget the stored records
for those shifts. Use the source keys printed in the report:

```bash
.venv/bin/teamup-shift-sync forget \
  --state .local/sync.sqlite3 \
  --shift 7e254c8bb3604748507391f8:2145427873:2145427873 \
           7e254c8bb3604748507391f8:2145427874:2145427874
```

`forget` only clears local memory of what was synchronized. It never contacts
MitHF or DUOS, and the next dry-run still has to be reviewed and approved before
anything is written.

Forgetting is not a force flag. With no stored step the planner falls back to
matching by value, so a destination record that still exists is adopted rather
than created twice, and an overlapping or ambiguous one still conflicts.

## Probe real TeamUp access (read-only)

For a real source preview, run `teamup-shift-sync dry-run --live --from
2026-09-14 --to 2026-09-20`. This reads events, descriptions, and comments
without creating
SQLite progress records. It shows resolved helper shifts and separate SPS
intervals. Destination reconciliation is explicitly **pending**, not assumed
empty: this preview cannot yet tell you which live records need creating.
Calendar capability keys are hashed before being used as report identities.

The CLI automatically reads supported TeamUp variables from `.env` beside the
config file or in its parent folder. The current `TEAMUP_API` variable supplies
the developer API key; `teamup.calendar_key` in ignored `config.toml` or a
`TEAMUP_CALENDAR_KEY` variable supplies the calendar reference.

```bash
.venv/bin/teamup-shift-sync teamup-probe \
  --from 2026-09-14 \
  --to 2026-09-20
```

The probe makes GET requests only. It reads the weekly event list, then the
official single-event endpoint for each occurrence so comment access is checked
explicitly. It prints counts only—never event titles, helper names, or comment
text. `TEAMUP_BEARER_TOKEN` is supported if the calendar key alone lacks read
permission.

## Supported SPS instruction grammar

Helper identity is resolved from the event's TeamUp subcalendar ID using
`teamup.helper_subcalendars` in local configuration. The seven helper mappings
provided by Jonas are configured locally. An event must match exactly one
helper subcalendar; zero or multiple matches require review. Destination
identities remain separate `helpers` entries keyed by the same subcalendar ID.
Source mappings alone do not establish MitHF names or DUOS employee numbers.

Titles starting with `Husk at checke …` are shared reminders, not shifts.
They are reported as excluded before helper matching or SPS parsing, in both
live and fixture previews. Matching ignores surrounding whitespace and case.

An exact `P-MØDE` title (ignoring case and surrounding whitespace) maps to
MitHF `Vagtmøde` for the event's interval. It does not imply SPS or DUOS hours.
Category application/read-back is pending; multi-helper events still require
assignment review.

SPS instructions are read from the event description, which is where the
calendar actually carries them, and from event comments. Both sources are
parsed; intervals from either are kept, and an interval written in both places
is planned once. One instruction per line:

```text
uni 8-10 & 13-14
uni 12-14 fredag
uni 2026-09-15 08:00-10:00 & 13:00-14:00
```

`&` preserves separate intervals. A day may be named with an ISO date or a
Danish weekday — full name or abbreviation, case-insensitive, with or without
`ø` and a trailing period (`fredag`, `fre`, `Fre.`, `lør`, `lor`). A named
weekday must resolve to exactly one local date inside the shift; a shift long
enough to contain that weekday twice requires an ISO date.

An undated instruction is accepted only when the source shift belongs to one
local date. For a shift spanning multiple local dates, name a day or the item
is flagged for review. Unsupported syntax, overlaps, times outside the source
shift, and ambiguous/nonexistent DST clock times are never guessed.

DUOS entries are planned only after their interval has ended. Multiple SPS
intervals remain independent DUOS steps; they are never merged into one
interval.

MitHF stores at most one SPS interval per shift, so a shift carrying several
intervals is planned as several consecutive MitHF shifts. Each cut is at the
start of the next interval: `uni 8-10 & 13-14` on a 07:30-24:00 shift becomes
07:30-13:00 with SPS 08:00-10:00, and 13:00-24:00 with SPS 13:00-14:00. The
parts cover exactly the original hours and leave no gap. An instruction that
could not be resolved never moves a boundary: the shift stays whole and the
SPS step is reported for review instead.

## Tests

The Rust migration in [#15](https://github.com/Jdreioe/teamup_sync/issues/15)
currently includes SPS parsing, source title rules, and compatible SQLite
storage. The desktop and CLI still use Python until the remaining planning and
integration slices are ported. See [migration progress](docs/rust-migration.md).

Run the Rust core and desktop tests from the repository root:

```bash
cargo test --workspace --all-targets
```

The existing Python suite uses unittest and the installed `tzdata` dependency,
which supplies timezone rules on systems without an IANA timezone database:

```bash
.venv/bin/python -m unittest discover -s tests -v
```

## Desktop shell

The Iced shell lives in `desktop/`. For development it starts the worker with
the repository virtual environment through `TEAMUP_WORKER_CMD`. Release bundles
place a standalone worker beside the GUI, so users do not need Python.

Desktop login needs the app-managed browser, so a development virtual
environment must include the `browser` extra:

```bash
.venv/bin/python -m pip install -e '.[dev,browser]'
```

Without it, both **Log ind** buttons report that the browser could not be
prepared. The packaged worker always bundles it.

Build and verify the portable bundle for the current operating system with:

```bash
.venv/bin/python -m pip install -e '.[package]'
.venv/bin/python scripts/package_desktop.py
```

The same worker handshake and fixture preview run in CI on Windows, macOS and
Linux. See `docs/desktop-validation.md` for the package and accessibility checks.

## The weekly preview

"Se ændringer" shows the selected Monday-to-Sunday week as a grid of seven day
columns, one block per MitHF shift over the hours it covers. A shift crossing
midnight appears in both days, marked as continuing, and keeps both dates in
its label. A shift split for several SPS intervals shows its parts. Under the
grid, the same week is repeated as text with the values the grid has no room
for, including old and new values for changes and the affected destination.

Blocks are filled with the helper's own TeamUp calendar colour, so the week
reads the way it does in TeamUp, and outlined in a status colour: new,
changed, unchanged or needs attention. Status never depends on colour alone —
each block also carries a text marker, and the day-by-day list below spells it
out. The guided setup reads colours from TeamUp automatically; a
configuration-driven run can set `teamup_color` per helper.

Items needing attention come first, each with a plain-language cause and the
next action. Empty weeks, unchanged weeks and an unread destination each read
differently, and an unread destination is never shown as empty.

"Overfør ændringer" is enabled only for a fully reconciled week with no
unresolved items and at least one write. Clicking it re-reads the source and
both destinations; only an unchanged plan counts as approved. Changing the
week, the setup or the plan revokes an approval. Running the transfer itself
is issue #6: this version approves a plan and sends nothing.

## Desktop login

Use **Log ind** beside MitHF or DUOS. The app downloads its own browser on first
use, then opens the service website for login and MFA. It saves each service's
session in a separate local profile and checks access read-only. A failed login
can be retried without changing the selected week. Passwords are entered only
on the service website. See `docs/desktop-validation.md` for validation limits.

## Live integration gates

Before apply mode can be enabled, the following must be completed:

1. Verify remaining TeamUp edge cases with real examples: a moved recurrence
   exception, and a shift overlapping the first boundary of the requested week.
   A real `uni` instruction in an event description is now covered.
2. Validate TeamUp helper identities against the configured MitHF and DUOS
   identities. Screenshot candidates are not treated as confirmed mappings.
3. Inspect authenticated MitHF and DUOS forms to obtain stable selectors and
   read-back behavior. In particular, MitHF SPS editing and DUOS save versus
   approval are still unknown.
4. Get authorization for the concrete live batch. Merely running a dry-run is
   never authorization to submit it.

## Guided desktop setup

Normal desktop launches now offer Danish, resumable setup and a live current-week
preview. See [guided setup](docs/guided-setup.md) for calendar access, the one-time
Teamup API key requirement, credential storage and validation limits. Fixture
mode is opt-in through `TEAMUP_FIXTURE`.
