# Rust core migration (#15)

Completed library slices:

- SPS parsing and source title rules (PR #17).
- SQLite state: step reads, durable writes, forgetting a source's steps,
  source/comment snapshots, legacy column upgrades, and apply locking.

`SyncState::open` accepts the existing SQLite path, including an account-scoped
`sync-<scope>.sqlite3` path. It preserves keys, statuses, destination IDs, source
hashes, errors, and recovery payloads. It does not choose or derive account paths;
the existing setup code remains responsible for that until setup is ported.
The schema is unchanged and SQLite is bundled into the Rust build.

Keep the `ApplyGuard` returned by `exclusive_apply` alive across the entire
transfer and read-back. Its lock uses the Python application's `.lock` file;
dropping the guard releases it, including during panic unwinding. SQLite calls
are blocking disk operations and must run outside the UI thread.

Source/comment hashes retain Python's sorted ASCII JSON and datetime formatting,
including null timestamps and six-digit microseconds. Removed comments remain
in the snapshot table, matching existing behavior. Step payload JSON may have
different whitespace; decoded values and the supplied source hash are preserved.
The snapshot hash helper is not a general approval-digest serializer.

Two intentional safety differences: a source snapshot is committed atomically
with its comments, and malformed/non-object step payloads return an error rather
than allowing reconciliation to treat the step as absent.

## Validation

```sh
cargo test -p teamup-shift-sync-core
```

The migration tests create a synthetic database with the current Python
`SyncState`, copy it, and compare Rust's snapshot output against Python's exact
stored rows. They also check Python reading Rust's recovery marker, old-schema
upgrades, separate account files, scoped forgetting, malformed payloads,
transaction rollback, and lock exclusion in both directions across processes.
No real account database or remote service is used.

These temporary parity tests require Python while both implementations exist.
They use `TEAMUP_TEST_PYTHON`, the repository virtual environment, or Python on
PATH; a missing interpreter fails the migration checks rather than skipping them.
The existing desktop CI runs these tests on Linux, Windows, and macOS. Local
Linux success alone does not establish Windows/macOS lock compatibility.

## Remaining work

Port planning/reconciliation and approval digests next, using the Rust state API.
Then port TeamUp access, destination/browser adapters, transfer orchestration,
and account/setup handling; connect both the CLI and Iced to those APIs. Python
remains the active application implementation during this staging work. The Rust
state module does not yet replace the Python state code in the running app.

Remove the Python application, worker protocol, runtime packaging and temporary
Python parity oracle only after integration and safety parity are established.
Update the architecture decisions and installation docs at that cutover.
