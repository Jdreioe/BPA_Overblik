# TeamUp shift sync

A local, manually triggered weekly importer for planning helper shifts from
TeamUp or a shared Google Sheet in MitHF. SPS registration in DUOS is optional.

One pure Rust core owns parsing, planning, approval, transfers and sync state.
The Iced desktop app and a small CLI both use it. There is no Python
installation, no worker process and no bundled runtime; see the completed
[migration](docs/rust-migration.md).

## Install it

Dated releases carry one file per system: a `.exe` for Windows 10/11, a
universal `.pkg` for macOS 11 and newer, and an `.AppImage` for 64-bit Linux.
Download the newest one from
[releases](https://github.com/Jdreioe/BPA_Overblik/releases/latest); the Danish
[installation guide](docs/installation.md) has the steps, including the
one-time security prompt on Windows and macOS.

A system Chromium is the only other requirement. The app never downloads a
browser. Updating and uninstalling both leave credentials, mappings and sync
history in place. How the packages are built, verified and versioned is in
[packaging](docs/packaging.md).

## Set up

A stable Rust toolchain is the only requirement.

```bash
cd /home/jdreioe/Projects/Vagtplanlaegning/teamup-shift-sync
cargo build --workspace
```

First-time setup happens in the desktop app, not in a config file:

```bash
cargo run -p teamup-shift-sync-gui
```

Connect TeamUp or Google Sheets, log in to MitHF, optionally enable DUOS,
map helpers, and review the week. Credentials live in the OS credential store;
setup and sync history live in the per-user application data directory
(`TEAMUP_SHIFT_SYNC_DATA_DIR` overrides it). Do not commit API keys, `setup.json`
or browser session data.

In **Indstillinger → Standardtider**, enter a shared shift time such as `6-22` if shifts
may have no times of their own. Each weekday can use that time, override it
(for example `8-20`), or say `ingen` to have no standard that day. Leaving the
shared field empty means there is no standard unless a weekday overrides it.
Both sources use the same rule: a single-day TeamUp all-day event on a confirmed
helper calendar, or a Sheets shift with a helper and an empty time field, uses
that date's standard. A Sheets row with separate start and end cells uses it
only when both cells are empty. The preview marks these shifts `standardtid`.
An unset weekday, a multi-day TeamUp all-day event, or a clock time invalid
on a daylight-saving transition requires correction before transfer.
Marker titles (below) are never shifts. Ordinary timed shifts keep their own hours.
Valid times save automatically. Changing the setting revokes approval of the
currently shown week. **Indstillinger** opens on **Hjælpere**; use its sidebar
for **Standardtider**, **Markeringer** and **Udbydere**. Under **Udbydere**, the **Vagtplan**
group holds the shift source (only one active at a time) and the **Løn** group
the MitHF and DUOS services, each with its own login. DUOS registration always
uses the ordinary shift type.

The desktop needs a system Chromium, or the `TEAMUP_BROWSER_PATH` executable.
It never downloads a browser.

## Run the representative dry-run

This fixture demonstrates an existing MitHF shift, one existing DUOS interval,
one new DUOS interval, an overnight shift, and an ambiguous undated SPS comment
on a multi-day shift.

```bash
cargo run -p teamup-shift-sync-cli -- dry-run \
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
successful step and never writes to external services. Add `--json` for a
machine-readable plan with its approval digest.

## Preview and apply a live week

Complete setup in the desktop app first then use its data directory:

```bash
cargo run -p teamup-shift-sync-cli -- dry-run --live \
  --data-dir ~/.local/share/teamup-shift-sync \
  --from 2026-09-14 --to 2026-09-20
```

Review every item. Resolve blocked items and preview again before applying.
Copy the exact `Plan digest` from the live preview into `--approve`:

```bash
cargo run -p teamup-shift-sync-cli -- apply \
  --data-dir ~/.local/share/teamup-shift-sync \
  --from 2026-09-14 --to 2026-09-20 \
  --approve YOUR_REVIEWED_64_CHARACTER_DIGEST
```

`apply` rereads the selected source and enabled destinations, rejects a changed approval, and
verifies every submitted step through read-back. If it fails after submission,
keep the database: the uncertain records support recovery. See the
[native Rust CLI](docs/rust-cli.md) for login, shape capture and exit codes.

## Re-synchronize a shift deleted by hand

If a MitHF shift or DUOS registration that was previously synchronized is
deleted directly in the destination, the plan reports it as `conflicted` and
will not recreate it. That guard is deliberate: a deletion is usually a decision
someone made on purpose, and the tool never silently undoes it.

When the deletion was a mistake and TeamUp should win, forget the stored records
for those shifts. Use the source keys printed in the report:

```bash
cargo run -p teamup-shift-sync-cli -- forget \
  --state ~/.local/share/teamup-shift-sync/sync-ACCOUNT.sqlite3 \
  --shift 7e254c8bb3604748507391f8:2145427873:2145427873 \
           7e254c8bb3604748507391f8:2145427874:2145427874
```

The desktop app offers the same recovery as **Tillad overførsel igen** on the
affected shift's conflict, for that shift alone.

`forget` only clears local memory of what was synchronized. It never contacts
MitHF or DUOS, and the next dry-run still has to be reviewed and approved before
anything is written.

Forgetting is not a force flag. With no stored step the planner falls back to
matching by value, so a destination record that still exists is adopted rather
than created twice, and an overlapping or ambiguous one still conflicts.

## Supported SPS instruction grammar

Helper identity is resolved from the confirmed TeamUp-to-destination mappings in
setup. An event must match exactly one mapped helper; zero or multiple matches
require review. Fixture runs map helpers through the TOML `helpers` list instead.

Titles starting with `Husk at checke …` are shared reminders, not shifts.
They are reported as excluded before helper matching or SPS parsing, in both
live and fixture previews. Matching ignores surrounding whitespace and case.

TeamUp events can also be calendar markers, such as a day-off wish. Their
titles are listed in **Indstillinger → Markeringer**, starting with
`Ønsker fri`. A title matches as a whole or as its first words, ignoring case
and spacing: `Ønsker fri - tandlæge` matches, `Ønsker fridag` does not.
`Husk at checke` reminders are always markers. The week shows each marker, on a
confirmed helper calendar, above the day it falls on. Markers are never
planned or transferred, and their text is never read for hours. A marker
never fails the week. When a marker overlaps a shift being transferred on the
same helper calendar, the week lists it under **Bemærk**. That note does not
block approval. Changing the list revokes approval of the shown week.
Sheets has no markers.

An exact `P-MØDE` title (ignoring case and surrounding whitespace) maps to
MitHF `Vagtmøde` for the event's interval. It does not imply SPS or DUOS hours.

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

DUOS entries are planned for past, ongoing and future intervals; only the
service's 26-hour limit is enforced at submission. Multiple SPS
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

```bash
cargo test --workspace --all-targets
```

Planning, approval, transfer, setup and destination-request tests assert against
golden files recorded from the previous engine before its removal, plus focused
Rust cases for cancellation, lock contention, corrupt state and UI transitions.
No interpreter, browser or live service is needed.

## Desktop shell

The Iced shell lives in `desktop/`. For development, run it with cargo; it
uses the same core and data directory as the CLI:

```bash
cargo run -p teamup-shift-sync-gui
```

`--self-check` builds the representative fixture preview headlessly and prints
its digest. CI runs it against every packaged bundle on Windows, macOS and
Linux. See `docs/desktop-validation.md` for the package and accessibility checks.

Release installers contain the desktop app, the CLI and `fixtures/`, and are
built by `scripts/package-*` from a dated tag. For a plain local build, use
`cargo build --release --workspace`.

## The weekly preview

The weekly preview shows the selected Monday-to-Sunday week as a grid of seven day
columns, one block per MitHF shift over the hours it covers. A shift crossing
midnight appears in both days, marked as continuing, and keeps both dates in
its label. A shift split for several SPS intervals shows its parts. Under the
grid, the same week is repeated as text with the values the grid has no room
for, including old and new values for changes and the affected destination.

For TeamUp, blocks use the helper's calendar colour. For Sheets, blocks
use a neutral fill. Both have a status outline: new,
changed, unchanged or needs attention. Status never depends on colour alone —
each block also carries a text marker, and the day-by-day list below spells it
out. The guided setup reads TeamUp colours automatically; a
fixture run can set `teamup_color` per helper.

Items needing attention come first, each with a plain-language cause and the
next action. Empty weeks, unchanged weeks and an unread destination each read
differently, and an unread destination is never shown as empty.

"Godkend ændringer" is enabled only for a fully reconciled week with no
unresolved items and at least one write. Clicking it re-reads the source and
enabled destinations; only an unchanged plan counts as approved. Changing the
week, the setup or the plan revokes an approval. The transfer verifies every
step through read-back and keeps recovery records for anything uncertain.

## Desktop login

Use **Log ind** beside MitHF or DUOS. The app opens the service website in an
app-owned browser window for login and MFA. It saves each service's
session in a separate local profile and checks access read-only. A failed login
can be retried without changing the selected week. Passwords are entered only
on the service website. See `docs/desktop-validation.md` for validation limits.

## Live integration gates

Live transfers need real-service verification behind each of these:

1. Verify remaining TeamUp edge cases with real examples: a moved recurrence
   exception, and a shift overlapping the first boundary of the requested week.
   A real `uni` instruction in an event description is now covered.
2. Validate TeamUp helper identities against the configured MitHF and DUOS
   identities. Screenshot candidates are not treated as confirmed mappings.
3. Complete live read-back verification for MitHF SPS editing and a separately
   authorized DUOS batch. A live SPS edit on a split shift was reported on
   2026-09-23, but its read-back status is not yet confirmed. A DUOS transfer
   only saves the citizen's
   registration. The helper accepts it later, and the app exposes no acceptance
   action.
4. Get authorization for the concrete live batch. Merely running a dry-run is
   never authorization to submit it.

## Guided desktop setup

Normal desktop launches now offer Danish, resumable setup and a live current-week
preview. See [guided setup](docs/guided-setup.md) for calendar access, the one-time
Teamup API key requirement, credential storage and validation limits. Fixture
previews are CLI-only.
