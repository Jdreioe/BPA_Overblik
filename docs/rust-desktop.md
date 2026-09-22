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

The home screen shows only the week, its changes and the two primary actions.
Everything technical lives on **Indstillinger** and **Hjælp**. Setup runs on
Indstillinger: until a setup is confirmed that is the screen the app opens, and
confirming revalidates both services and returns to the week. Importing a legacy
TOML configuration is the one setup feature not carried over from the old engine.

1. On **Indstillinger**, choose **Log ind i MitHF** and **Log ind i DUOS**.
   Complete authentication in each browser. These two buttons are the only thing
   that opens a browser window; reads reuse the saved profile headless, and
   MitHF's shift calendar is entered through the site's own "Åbn din vagtplan"
   action. Login lives beside setup because reading the catalog needs both
   services.
2. Choose **Kontrollér login**, then return to the week and **Se ændringer**.
3. Review the split shifts, times, helper assignments, SPS and DUOS values.
   Resolve any points under **Kræver opmærksomhed** before proceeding.
4. **Overfør ændringer** approves exactly the displayed plan and submits it.
   The app reloads saved setup and TeamUp, checks the account database scope,
   and invokes the core's locked destination revalidation and verified transfer.

Opening Indstillinger revokes a shown approval, because editing settings can
change the account behind it.

## Switching accounts

Synchronization history is stored per account in `sync-<scope>.sqlite3`, where
the scope covers the calendar, the MitHF customer and grant, the DUOS
arrangement and registration type, and the confirmed helper mappings. Every
preview and every transfer rereads both destinations and refuses to continue
unless the fresh catalog still matches the confirmed one, so one account can
never plan against another's history. A transfer additionally refuses an
approval whose account database is no longer the current one.

**Log ud af MitHF og DUOS** on Indstillinger closes the app's browsers and
removes only its own profiles, so a different account starts from a clean
session. It keeps the synchronization history and changes nothing in either
service.

## Recovery and diagnostics

When a previously transferred MitHF shift or DUOS registration has been deleted
by hand, that shift's item under **Kræver opmærksomhed** offers **Tillad
overførsel igen**. Confirming forgets this app's own records for that one shift,
through the same `forget_steps` path and apply lock as the CLI, and revokes the
preview. Nothing is deleted in a destination, no other shift is affected, and
the next preview still runs the full overlap and conflict checks before anything
can be approved.

**Hjælp** holds short Danish guidance and two exports. Neither is called a
diagnostic on screen: the person asked to produce one is not a developer.

**Del hvad der gik galt** writes `fejlrapport.json` and opens a prefilled issue
on this repository in the person's own browser. The report holds app and system
versions, how far setup reached, per-account step counts by status, the last
verification time, and whether the browser and its profiles are present. It is
counts and yes/no answers only, with no credential, cookie, calendar link,
service identifier, name, shift text or raw service response, and it needs no
service or confirmed account, so it also works while setup is stuck.

The app does not publish the report: there is no token and no issues API
call, only a `…/issues/new?title=&body=` address that the browser opens. The person reads the
filled form and presses Submit themselves, and the screen says beforehand that
the issue is public and needs a GitHub account. A report too long for an address
is left out of it, and the form then asks for the saved file instead. If no
browser can be opened, the file is still written and the app says to send it.

**Gem en fejlrapport om MitHF og DUOS** writes `fejlrapport-tjenester.json`,
which records the field types the two services return, without any values. It is
the same capture the CLI writes.

The native workflow uses the same separate browser profiles as the Rust CLI.
Only one native browser owner can run at a time. Browsers belong to that
window and close when it exits. Use `TEAMUP_BROWSER_PATH` if Chromium cannot
be found; this mode does not download a browser.

On startup the app asks GitHub for the latest dated release, and while the
window stays open it asks again every six hours. A newer package shows a banner
with **Hent opdatering**. Windows and Linux download and replace the running
file, then reopen it. macOS downloads the `.pkg` and opens the installer.
**Tjek for opdateringer** on Indstillinger runs the same check by hand. A
development build (`0.1.0`) never offers an update. Failures on the automatic
check stay silent.

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
previews, setup reload and changes attempted during apply. Further tests cover
screen navigation, that only a missing destination entry offers allowing a
transfer again and only from its own conflict, that forgetting local records
revokes the preview, and that the diagnostics report contains no account
content.

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
