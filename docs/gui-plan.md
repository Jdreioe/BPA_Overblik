# A simple desktop app for weekly transfers

Draft, 19 September 2026. Jonas selected Iced and cross-platform support for
Windows, macOS and Linux.

Tracking issue: https://github.com/Jdreioe/teamup_sync/issues/1

- [ ] [Build the Iced desktop shell on the pure Rust core](https://github.com/Jdreioe/teamup_sync/issues/2)
- [ ] [Manage browser login sessions without manual browser setup](https://github.com/Jdreioe/teamup_sync/issues/3)
- [ ] [Add guided setup with calendar discovery and confirmed helper matches](https://github.com/Jdreioe/teamup_sync/issues/4)
- [ ] [Show a readable weekly preview and bind approval to its exact changes](https://github.com/Jdreioe/teamup_sync/issues/5)
- [ ] [Run approved transfers with verified progress and safe recovery](https://github.com/Jdreioe/teamup_sync/issues/6)
- [ ] [Add simple settings, account isolation and targeted conflict recovery](https://github.com/Jdreioe/teamup_sync/issues/7)
- [ ] [Ship and validate the cross-platform app with nontechnical users](https://github.com/Jdreioe/teamup_sync/issues/8)

## Product decision

Build a locally installed Rust/Iced desktop app around a pure Rust sync core.
The user installs it, connects their services once, and reviews a week before
transferring it. No terminal, runtime installation, configuration editing, browser
debugging address, or employee-number lookup should be part of normal setup.
Keep account data and synchronization records on the user's computer. Do not add
a hosted service, another account, scheduled transfers, or a calendar editor.

Use Danish labels initially, matching the destination services. Use a readable
single-column layout, clear keyboard focus, large buttons, and text alongside
status colors. Each screen has one primary action. Technical diagnostics belong
under Help, not in the normal workflow.

## First launch

1. **Tilslut TeamUp.** Paste the calendar link. Resolve the calendar reference
   and list available helper calendars. Validate access to events and comments.
   Import existing local configuration when available, with a readable summary.
2. **Log ind.** Separate MitHF and DUOS buttons open their real login pages in
   an app-managed browser. The app detects successful access and remembers the
   browser profile. Users complete authentication themselves, including any MFA.
3. **Bekræft hjælpere.** Show TeamUp, MitHF, and DUOS names side by side. Suggest
   unique matches, ask users to confirm them together, and show dropdowns for
   ambiguous matches. Persist stable identifiers internally. Let users exclude
   calendars that are not helper calendars. Never silently guess identities.
4. **Bekræft ordning.** Show readable destination account, arrangement and
   registration-type choices. Suggest uniquely eligible values, but require
   confirmation of the intended destination and DUOS type. Multiple MitHF
   customers or grants need an explicit choice or a clear unsupported message.
5. **Se ugens ændringer.** Save setup and open the current week. Setup resumes
   where the user left off if they close the app.

The TeamUp developer API key is an unresolved distribution requirement. The
current engine requires it in addition to a calendar reference. Investigate a
supported provisioning route before promising calendar-link-only setup. Prefer
maintainer-assisted provisioning to asking every user to register as a developer.
Never embed a personal secret in a public installer. If manual entry remains
necessary, provide a guided one-time field and document the limitation clearly.

## Normal use

The home screen shows service connection status, the last verified transfer,
and the current Monday-to-Sunday week. Previous/next arrows and "Denne uge"
cover normal navigation. The primary button is "Se ændringer".

The preview shows day, helper and shift time, followed by what will happen in
MitHF and DUOS. Separate SPS intervals remain separate. Overnight shifts show
both dates. Updates show old and new values. Count user-visible shifts and
registrations, not internal API calls.

Put items requiring attention first. Each explains the problem and the next
action, such as correcting an instruction in TeamUp or reconnecting a service.
Already matching entries are collapsed. Empty weeks and weeks with no changes
have distinct messages. An unread destination must never appear empty.

The primary button becomes "Overfør ændringer" with a readable summary of the
destinations and changes. That click approves exactly the displayed plan; the
app carries the digest internally. Changing dates, settings, mappings or source
data invalidates that approval. Refresh source and destinations before applying.
A changed plan returns to review rather than transferring unexpected changes.
Keep the existing whole-batch blocking rule in v1. No partial-selection feature.

During transfer, show the current destination and verified progress. Prevent
duplicate runs. Success appears only after read-back verification. If interrupted,
show what was verified and what needs checking. "Kontrollér igen" reconciles
remote state before offering another reviewed transfer. Never blindly retry an
uncertain submission. Closing during a transfer must explain that completed
writes remain and allow work to stop safely between operations.

## Secondary screens

- **Indstillinger:** reconnect services, change the calendar, edit helper
  mappings and arrangement choices. Scope local state to the connected accounts
  so switching accounts cannot reuse another account's approvals or sync history.
- **Hjælp:** short Danish guidance and explicit export of redacted diagnostics.
  Never include tokens, cookies, calendar capability links, names or shift text.
- **Recovery:** on a conflict caused by a manually deleted destination entry,
  offer "Tillad overførsel igen" for that shift. Explain that it forgets local
  sync history only. Require confirmation, then a fresh preview and approval.

## Implementation boundaries

Reuse the Rust core (`core/`) from both CLI and GUI; do not parse CLI output.
Run network/browser work away from the UI thread and provide structured progress
and errors. Keep the existing CLI usable.

Use Iced as requested: https://iced.rs/. Keep business rules in the core, UI
code outside it. The desktop and the CLI call the same core planning and
transfer functions. The Python worker and its JSON protocol were removed in
#15 once parity was established; do not reintroduce a second engine.
Keep diagnostics separate from redacted user-facing messages. Define typed
messages for preview, approval, progress and errors. Detect adapter failure and
reconcile before resuming. Do not introduce a local HTTP service.

The first delivery task proves Iced accessibility, binary packaging and browser
lifecycle on Windows, macOS and Linux, including any platform limitations.
Prove installation on a clean machine early. Bundle or manage the required
runtime/browser without shell commands. Use an isolated app browser profile;
do not require changes to the user's everyday browser or an exposed debug port.
Store secrets using the OS credential store and protect session/profile files.
Keep SQLite state across upgrades. Never reset uncertain-write records to fix
an installation or login problem.

The README currently describes an older offline milestone, while the code has
live destination reads and apply logic. Validate actual capabilities before
enabling them in the GUI. Preserve blockers for multiple SPS intervals, ambiguous
helper matches, incomplete reads and unsupported destination operations. A DUOS
transfer saves the citizen's registration as pending. The helper accepts it
separately, and the app must not expose that acceptance action.

## Delivery and acceptance

Create one tracking issue and seven implementation issues, in this order:

1. Desktop shell, shared application operations and clean-machine package proof.
2. App-managed login sessions and reconnect flow.
3. Guided setup, credential provisioning and helper/account mapping.
4. Readable weekly preview and exact-plan approval.
5. Verified transfer, interruption handling and safe retry.
6. Settings, account isolation and targeted recovery.
7. Installable release, accessible Danish flows and nontechnical usability check.

Login work follows the shell. Setup follows login. Preview can begin with offline
fixtures after the shell, then connect to setup. Transfer follows preview and
login. Recovery follows setup and transfer. Release validation covers all paths.

Acceptance requires a person unfamiliar with the repository to install and
reach a correct preview without a terminal or editing files. After setup, an
ordinary unchanged-login week should take two primary actions from home:
"Se ændringer" and "Overfør ændringer". Measure first-run friction and fix any
step requiring an explanation of API keys, CDP, TOML, hashes or employee IDs.
If TeamUp provisioning cannot meet that target, record the precise exception.

Use focused engine tests for approval invalidation and interrupted/uncertain
writes, fixture-backed UI checks for preview and errors, and a clean-machine
installation check on each supported OS. Validate screen-reader labels,
keyboard-only operation, focus, readable scaling and non-color status cues.
Any live write test requires a separately authorized concrete batch; design and
issue creation do not authorize calendar or time-registration changes.
