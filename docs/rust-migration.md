# Rust core migration (#15)

Complete. The Python engine, worker protocol, runtime packaging and parity
oracles are removed. The desktop, the CLI and all tests run on the Rust core
with no Python installation.

Completed slices:

- SPS parsing and source title rules (PR #17).
- Read-only planning and reconciliation, including split SPS shifts, helper
  assignment, meeting categories, DUOS status checks, and retired segments.
- Asynchronous transfer orchestration with apply locking, durable recovery
  markers, per-step revalidation and verified read-back.
- Approval and recovery-payload hashes, batch validation, checks before each
  step, write read-back validation, and final reconciliation checks.
- SQLite state: step reads, durable writes, forgetting a source's steps,
  source/comment snapshots, legacy column upgrades, and apply locking.
- Live adapters: TeamUp reads, browser-backed MitHF and DUOS reads, and the
  destination write adapter that `apply_plan` drives.
- Setup state machine: document loading and saving, TeamUp connection,
  catalog refreshes, arrangement and helper choices, validation and
  confirmation, sharing the account scope with the transfer path.
- Native CLI: fixture and saved-setup live previews, browser login, approved
  transfers, and scoped forgetting of local recovery records.
- Iced desktop: saved-setup loading, guided setup, browser login, Danish week
  presentation, reviewed transfers and recovery feedback, all on the core.

`SyncState::open` accepts the existing SQLite path, including an account-scoped
`sync-<scope>.sqlite3` path. It preserves keys, statuses, destination IDs, source
hashes, errors, and recovery payloads. Account paths come from the shared
`account_scope`, so history written by the old engine keeps working.
The schema is unchanged and SQLite is bundled into the Rust build.

Keep the `ApplyGuard` returned by `exclusive_apply` alive across the entire
transfer and read-back. Its lock uses the application's `.lock` file;
dropping the guard releases it, including during panic unwinding. SQLite calls
are blocking disk operations and must run outside the UI thread.

Source/comment hashes retain the previous engine's sorted ASCII JSON and datetime formatting,
including null timestamps and six-digit microseconds. Removed comments remain
in the snapshot table, matching existing behavior. Step payload JSON may have
different whitespace; decoded values and the supplied source hash are preserved.
The snapshot hash helper is not a general approval-digest serializer.

Two intentional safety differences: a source snapshot is committed atomically
with its comments, and malformed/non-object step payloads return an error rather
than allowing reconciliation to treat the step as absent.

## Planning

`build_plan(&PlanRequest, &SyncState)` returns a plan without changing state.
`PlanningConfig` contains only the configuration the planner uses, with a parsed
IANA timezone and confirmed helper mappings. It does not carry credentials.
The planner preserves legacy step keys, payload datetime formatting, action
ordering, conflict reasons, and uncertain-write recovery rules.

Adapters must supply a complete `DestinationSnapshot` for the bounds returned
by `reconciliation_range`. Partial reads must fail before planning. Planning alone never authorizes a write.
Run planning outside the UI thread because it reads SQLite.

The planner fails on invalid stored segment keys or payloads instead of returning
a partial plan. It reads all steps for each selected, mapped source in one query.
Parser diagnostics use Rust quoting; parity checks compare their codes, source IDs,
outcomes and reasons, but not that diagnostic wording. Planner-generated messages
are compared exactly.

## Approval and apply checks

`plan_digest` matches the previous engine's SHA-256 approval hash. It binds the selected range,
ordered source/step keys, outcomes, destination IDs and payloads. It excludes
summaries, reasons, system labels and generation time, as before.
`payload_digest` matches the hash stored in each step's `source_hash` field.
Both preserve sorted keys, ASCII JSON escapes, Python-compatible JSON spacing and datetime
precision. Current transfer payloads contain no floats. Rust rejects floating-point
values with `UnsupportedNumber` rather than risk a different hash. This is an
intentional restriction for unsupported payloads, not a digest change
for current plans.

`ApprovedPlan::validate` checks a freshly reconciled plan against the approved
digest, rejects unresolved items, and rejects different sources claiming the same
MitHF shift. Its borrowed plan cannot change while the approval is in use.
`check_step` checks each approved item against another fresh plan and returns
`Skip`, `AlreadyMatched` or `Write`. It rejects changed payloads and new unapproved
writes, while allowing destination IDs to appear or change after creation or
assignment. `check_read_back` requires the submitted step to be already matched;
`check_final` requires all final items to be matched or excluded, as before.

These checks remain read-only. The `apply_plan` runner integrates them with
persistence and adapters as described below.

## Transfer orchestration

`apply_plan(ApplyRequest, state_path, destinations, progress)` runs on a Tokio
runtime. Its `Destinations` trait exposes asynchronous reads and writes;
`live::LiveDestinations` is the real implementation. Callers provide the
account-scoped state path and an approval digest from their reviewed live plan.
The runner opens its own state connection and holds the apply lock from before
the initial destination read through the final source snapshot commit.

The source batch, configuration, range and planning time remain fixed during a
run. Every non-excluded step gets a fresh complete read and revalidation. The
runner commits an uncertain marker before submitting a write, saves any returned
ID, then reads again and requires an already-matched result before marking the
step verified. As before, a changed ID returned by helper assignment updates
that segment's parent creation record while preserving its payload and hash.
Source snapshots are recorded only after successful final reconciliation.

All database operations run on Tokio's blocking pool. A blocking job retains the
apply lock even if the calling future is cancelled before that job completes.
Cancellation or adapter errors leave committed uncertain markers intact; adapters
must not launch detached writes after cancellation. Progress reports only verified
step keys, and callbacks must return promptly. Destination failures use a generic
user-facing message; adapters must keep secrets and source content out of errors.

## Validation

```sh
cargo test -p teamup-shift-sync-core
```

The migration tests open a SQLite database the previous engine wrote — frozen
as `core/tests/goldens/python-created.sqlite3` — and compare Rust's snapshot
output against its exact stored rows. They also check old-schema
upgrades, separate account files, scoped forgetting, malformed payloads,
transaction rollback, and lock exclusion between two Rust handles.
The planning goldens hold 263 safety combinations and the representative week
recorded from the old planner, compared with the exact stored rows. These cover duplicate and overlapping destinations, missing records, manual
edits, uncertain writes, rejected/non-pending DUOS registrations, SPS conflicts,
meeting categories, removals, incomplete source instructions, range boundaries,
and timestamp precision. A native Rust test verifies that planning retains
recovery records and fails on corrupt state.

The same recorded cases compare approval digests. Approval tests
also replay plan traces from real apply runs, covering repeated runs,
split shifts, stale approvals and lost write responses. Additional hash cases
cover nested objects, Unicode, ASCII control characters and integer limits;
focused Rust checks cover batch blockers, shared destination claims, payload
changes, unapproved writes and failed verification.

Transfer tests execute complete Rust runs against a synthetic asynchronous
adapter, then compare write order, destination records, and persisted recovery
records with the recorded integration scenarios. A separate connection verifies that
each uncertain marker is committed and the apply lock is held before submission.
Focused cases cover cancellation, competing applies, incomplete reads, failed
read-back, final-read failure, meeting categories and full-shift read bounds.

No real account database or remote service is used.

The desktop CI runs these tests on Linux, Windows, and macOS. Local
Linux success alone does not establish Windows/macOS lock compatibility.

## Live adapters

`live` holds the native TeamUp, MitHF and DUOS integrations. `load_saved_setup`
reads the desktop app's existing `setup.json` and OS vault without changing
either, and derives the same account-scoped state path. It never writes setup.

`BrowserSessions` owns only the browsers it launches, under a separate profile
root, so running it cannot disturb the previous app's live sessions. It exposes
two transports. `request` accepts read actions only and is public; `submit`
accepts the five MitHF write actions and the DUOS `register` action and is
crate-private, so the only way to reach a write is through the transfer
adapter with an approved plan item. The page script already restricted the same
action set; the split makes the read-only preview unable to write by type.

`LiveDestinations::connect` resolves MitHF and DUOS identities once against
today, exactly as the previous engine did before an apply, and implements `Destinations`
for `apply_plan`. The per-step reads that follow never re-resolve identity:
the selected week scopes which records are read, never whether a helper,
arrangement or employment is valid. Building a request is a pure function of
the plan item, the snapshot and the resolved identities, so every submitted
body matches the recorded request from the previous engine.

Two differences from the previous engine, both deliberate:

- Adapter messages are the Danish user-facing strings the rest of `live` uses,
  not the previous engine's English developer text. `LiveError` carries no response content.
- `mithf.set_meeting` reads the segment bounds its planner payload actually
  carries. The previous engine's writer read an `intervals` key that its own planner never
  puts in a meeting payload, so a live P-møde write raised there. The Rust
  path is the corrected one.

The 26-hour interval rule for DUOS is checked at submission against the
interval duration alone; future and ongoing intervals are accepted.

### Validation

`write_requests_match_recorded_cases` compares the exact submitted service, action
and body for twelve cases recorded from the previous engine: DUOS creation, update, and both daylight-saving
transitions; MitHF creation, overnight creation, time correction, helper
assignment, and SPS category creation and update. Focused Rust checks cover the
DUOS interval rule, single-helper creation, refusing to collapse several
existing category records, the meeting payload above, and update without a
read-back parent. No network, browser or real account is used.

The read adapters are not covered by automated tests; they need live services.

## Native CLI

`cli/` builds `teamup-shift-sync-rust`, sharing the core fixture, planning
and transfer paths with the desktop. See [CLI usage](rust-cli.md). Live commands require an explicit desktop
data directory and use its existing ready setup, OS vault credentials and
account-scoped database. They do not accept a state-path override. Setup is
completed in the desktop app.

`dry-run --live` reads TeamUp and both destinations, expands destination bounds
to cover complete selected shifts, and prints the plan and its approval digest.
`apply` requires explicit inclusive dates and that digest. It rereads TeamUp,
then calls the core transfer runner, which rereads destinations under the apply
lock and rejects changed or blocked plans before writing. Live commands use the
wall clock; only fixture previews accept `--now`.

`login` opens the existing native adapter's separate Rust profiles, waits for
the user to finish authentication, and checks read access. Browsers belong to
the command and close when it exits. No command downloads a browser; use the
desktop's installed browser or `TEAMUP_BROWSER_PATH`.

Fixture previews read the existing TOML config and JSON fixture format. All
three fixture lists are required to avoid treating omitted destinations as
empty. They may initialize a SQLite schema, but never record transfer steps or
source snapshots. Exit codes are 0 for success, 1 for a blocked preview or no
matching records to forget, and 2 for invalid arguments or operational failure.
The default fixture database is `.local/offline-rust.sqlite3`.

CLI integration tests compare the representative preview digest and outcomes
with the frozen recording, check both daylight-saving transitions and invalid inputs, and
verify scoped forgetting with lock contention. All use synthetic inputs and
temporary state. Live CLI authentication, preview and submission have not been
exercised against real accounts.

## Setup

`live::Setup` owns the setup document: loading, atomic saving, the TeamUp
connection, catalog refreshes, arrangement and helper choices, validation and
confirmation. It writes `setup.json` and the OS keyring entry; the document
itself never contains a secret, only the handle its credentials are stored
under. The Iced setup screen reads `Setup::view()` directly from the core.

`build_catalog` is the destination catalog both setup and the live read path
use, so a confirmed catalog and the one revalidated before a transfer cannot
drift apart. `source_catalog` lists TeamUp calendars and confirms the link
exposes shift details and comments. Colour is kept out of `calendars`, so
recolouring a calendar never discards a confirmed mapping or an approval.

`account_scope` derives the `sync-<scope>.sqlite3` name and is shared by setup
and `load_saved_setup`. It matches the previous engine exactly, as checked by
the frozen scope golden: a different
scope would orphan an existing history and re-submit completed work.

Importing a legacy TOML configuration is deliberately not ported.

### Validation

`the_setup_state_machine_matches_recorded_scenarios` replays recorded
steps from the previous engine against the Rust state machine, comparing the whole document and the
exact Danish error text after each one. It covers refreshes that preserve or
reset mappings, renamed calendars, a changed destination catalog, automatic
selection of a single arrangement, ambiguous MitHF names, and half-reviewed
confirmations. Only the two catalog readers are stubbed, on both sides.

The service-contacting halves are not covered; they need live accounts.

## What remains outside this migration

The desktop runs the core directly, and the CLI is available separately.
Release packaging is a cargo build plus the fixture self-check; no Python
runtime ships. A Rust live destination write was performed and verified against
a real account before the cutover: the transfer ran the full loop, including
the committed uncertain marker, read-back and final reconciliation.
Per-window progress reporting and the
distinction between verified, uncertain and remaining work after a partial
failure belong to #6.

The live read adapters still have no automated coverage, and the first real
account run found two fields typed as JSON booleans in Rust (`valgbar`,
`daekket`) that the previous engine reads as plain truthy values.
`cli capture` records the JSON structure of every service read — types only, no
values — so this class of mismatch can be checked before a live run rather than
during one. Run it once per account and commit the result; see
[CLI usage](rust-cli.md).
