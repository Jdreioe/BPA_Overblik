# Desktop app

The Iced binary runs the native workflow directly on the Rust core. It reads
TeamUp, builds the Danish week view, and applies reviewed changes without any
worker process.

```sh
cargo run -p teamup-shift-sync-gui
```

It uses the per-user application data directory, or `TEAMUP_SHIFT_SYNC_DATA_DIR`
when set, for `setup.json`, browser profiles and sync history. TeamUp
credentials are read from and written to the OS vault.

Setup runs in this window too. Until a setup is confirmed, the app shows the
connection, arrangement and helper steps instead of the week view. Confirming
moves straight to the week view. Importing a legacy TOML configuration is the
one setup feature not carried over from the old engine.

1. Choose **Log ind i MitHF** and **Log ind i DUOS**. Complete authentication
   in each browser. These two buttons are the only thing that opens a browser
   window; reads reuse the saved profile headless, and MitHF's shift calendar
   is entered through the site's own "Åbn din vagtplan" action. Login is
   available during setup as well, because reading the catalog needs both
   services.
2. Choose **Kontrollér login**, then select a week and **Se ændringer**.
3. Review the split shifts, times, helper assignments, SPS and DUOS values.
   Resolve any points under **Kræver opmærksomhed** before proceeding.
4. **Overfør ændringer** approves exactly the displayed plan and submits it.
   The app reloads saved setup and TeamUp, checks the account database scope,
   and invokes the core's locked destination revalidation and verified transfer.

The native workflow uses the same separate browser profiles as the Rust CLI.
Only one native browser owner can run at a time. Browsers belong to that
window and close when it exits. Use `TEAMUP_BROWSER_PATH` if Chromium cannot
be found; this mode does not download a browser.

Operations run asynchronously. Controls are disabled while an operation is
running, so the selected account and week cannot change during a transfer.
The approval is consumed when transfer starts. During a transfer the app shows
the approved scope, current destination and a progress bar that advances for
each verified shift or registration. MitHF's internal writes count as one
shift. Each DUOS interval counts as one registration. Success is shown only
after the core verifies the entire batch, with per-destination counts and the
verification time in the configured local timezone. The home screen keeps
showing that account's time until the account changes.

On failure, the app distinguishes verified, uncertain and not-started work.
"Kontrollér igen" re-reads TeamUp and both destinations before another approval
is possible. Stopping or closing during a transfer requests a stop between
operations; an in-flight operation completes its read-back first. A process
termination can still leave its durable uncertain marker for reconciliation.

A DUOS transfer saves the citizen's registration. It remains `Afventer` until
the helper accepts it. The app has no DUOS acceptance action and cannot accept a
registration for the helper.

The packaged bundle is checked headlessly: `teamup-shift-sync-gui --self-check`
builds the representative fixture preview through the core and prints its
approval digest. It uses a throwaway state file, so the check never touches a
live account database.

## Validation

```sh
cargo test -p teamup-shift-sync-gui --all-targets
```

The frozen golden files compare the native week view against 16 Python-era
presentation scenarios. They cover split shifts, colours, overnight
continuations, old/new values, helper/category-only writes, blocked instructions,
future DUOS hours, and empty, unchanged and unread weeks. Invalid display times
fail the preview. UI state tests check approval consumption, blocked and stale
previews, setup reload and changes attempted during apply.

On Linux, an Xvfb check rendered the missing-setup screen with a temporary empty
data directory. This verifies startup and its setup error screen, not
authenticated service behavior.

Frozen goldens also compare the setup state machine step by step: what
a refresh preserves, what a changed catalog resets, when an arrangement is
chosen automatically, which helpers may be proposed, and every Danish
validation message. They also check that the account scope naming
`sync-<scope>.sqlite3` matches the previous engine exactly, so a setup written
by Rust keeps the existing transfer history instead of starting an empty one.

The setup screen itself has not been rendered under test; there is no display
in the development environment. `connect`, `refresh_source`, `discover`,
`revalidate` and `confirm` all contact live services and are not covered.
Windows and macOS execution remain for CI and platform validation.
