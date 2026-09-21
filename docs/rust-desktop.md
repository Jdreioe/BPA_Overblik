# Native desktop mode

The Iced binary has an opt-in native workflow for accounts that already have
confirmed setup. It reads TeamUp, builds the Danish week view, and applies
reviewed changes through the Rust core without starting a Python worker.

```sh
cargo run -p teamup-shift-sync-gui -- --native
```

It uses the same desktop data-directory selection as the existing window.
Set `TEAMUP_SHIFT_SYNC_DATA_DIR` to the directory containing your `setup.json`
if needed. TeamUp credentials are read from and written to the OS vault.

Setup runs in this window too. Until a setup is confirmed, the app shows the
connection, arrangement and helper steps instead of the week view, sharing the
same screen as the default desktop but driven by the Rust core rather than the
Python worker. Confirming moves straight to the week view. Importing a legacy
TOML configuration is the one setup feature not carried over.

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
The approval is consumed when transfer starts. Success is shown only after
the core verifies the entire batch. Failure keeps recovery records and requires
a new preview before another transfer.

The normal window and packaged self-check remain unchanged. Native mode refuses
`TEAMUP_FIXTURE` and `--self-check` rather than interpreting an offline run as a
live account workflow.

## Validation

```sh
cargo test -p teamup-shift-sync-gui --all-targets
```

The temporary Python oracle compares the native week view against 16 existing
and daylight-saving scenarios. It covers split shifts, colours, overnight
continuations, old/new values, helper/category-only writes, blocked instructions,
future DUOS hours, and empty, unchanged and unread weeks. Invalid display times
fail the preview. UI state tests check approval consumption, blocked and stale
previews, setup reload and changes attempted during apply.

On Linux, an Xvfb check rendered the missing-setup screen with a temporary empty
data directory and an unavailable Python worker. This verifies native startup
and its setup error screen, not authenticated service behavior.

A temporary Python oracle compares the setup state machine step by step: what
a refresh preserves, what a changed catalog resets, when an arrangement is
chosen automatically, which helpers may be proposed, and every Danish
validation message. It also checks that the account scope naming
`sync-<scope>.sqlite3` matches Python exactly, so a setup written by Rust keeps
the existing transfer history instead of starting an empty one.

The setup screen itself has not been rendered under test; there is no display
in the development environment. `connect`, `refresh_source`, `discover`,
`revalidate` and `confirm` all contact live services and are not covered.
Windows and macOS execution remain for CI and platform validation. Runtime
packaging changes and removal of the Python implementation remain separate
migration work.
