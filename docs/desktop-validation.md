# Desktop validation

Issue #2 uses the same package check on Windows, macOS and Linux. The CI matrix
builds a standalone Python worker, places it beside the Iced executable, and
runs `teamup-shift-sync-gui --self-check` without a Python command on the
worker path. The check performs the protocol handshake, loads home status, and
builds the representative fixture preview. The resulting portable directory is
uploaded as a zip artifact.

Run the same check locally with:

```bash
python -m pip install -e '.[package]'
python scripts/package_desktop.py
```

## Accessibility check

The shell uses native Iced buttons with visible text, 12 to 14 pixels of
padding, text descriptions alongside every status, and no color-only meaning.
Loading, empty, failure, and worker-recovery states are part of the Rust widget
tests. Before closing issue #2, record one manual pass for each packaged OS:

- Tab and Shift+Tab reach the week controls, primary action, and retry action
  in reading order. Enter and Space activate the focused button.
- The platform screen reader announces the window title, button text, week,
  service status, preview summary, and errors.
- Focus remains visible at 100% and 200% display scaling, with the content
  reachable by scrolling at a small window size.

Known limitation: Iced 0.14 exposes button text and focus through its platform
accessibility integration, but this first shell has no explicit heading or
live-region semantics. Progress announcements belong to the transfer screen in
issue #6.

## Weekly preview and approval (issue #5)

The worker returns the week already translated: day labels, block geometry in
minutes from local midnight, Danish status labels, detail lines and the
attention list. The shell renders those fields and never parses summaries,
step keys, digests or outcome names. The block fill is the helper's TeamUp colour,
tinted towards white so dark text stays readable on Teamup's saturated
palette; the outline carries status. Blocks also carry a text marker (`[NY]`,
`[ÆNDRET]`, `[OK]`, `[!]`), so neither identity nor status depends on colour
alone, and the grid is repeated as text below it for screen readers.

Colour is appearance, not identity: it is kept out of the sub-calendar
comparison in `Setup.revalidate`, so recolouring a calendar in TeamUp adopts
the new colour without discarding confirmed mappings or an approval.

Python tests cover the split, overnight days, future DUOS hours, changed
values, empty and unchanged weeks and an unread destination. Rust tests cover
approval requiring an unchanged re-read, a changed or newly blocked plan
returning to review, and revocation on week, setup or retry changes.

Fixture mode can never enable a transfer: it reconciles no destination, so
`can_apply` stays false and the primary action stays disabled with its reason.

## Browser sessions (issue #3)

The home screen now offers separate MitHF and DUOS login buttons. On first use,
Playwright downloads its matching Chromium into the per-user app data directory.
The UI shows an indeterminate download stage, then opens the real service site.
Download and launch failures offer another login attempt. The packaged worker
includes Playwright and its installer; users need no Python or terminal command.
The implementation follows Playwright's [browser installation](https://playwright.dev/python/docs/browsers)
and [persistent context](https://playwright.dev/python/docs/api/class-browsertype#browser-type-launch-persistent-context)
APIs.

Each service has a separate persistent profile under `profiles/`. Browser control
uses Playwright's private pipe, with no TCP debugging listener. Login and MFA
happen in the visible service browser. A background thread checks access through
read-only helper-list/portfolio requests; only fixed status messages reach Iced.
The selected week and setup survive reconnecting either service. Closing the app
closes its browser contexts after pending operations finish. It never attaches to
or closes a normal browser. The CLI's existing CDP mode remains separate.

Automated checks cover read-only probes, MFA redirects, expired sessions,
redacted failures, duplicate clicks, closed browsers, and service isolation.
Before closing #3, manually verify on Windows, macOS and Linux:

- First download, failed download and retry, including a machine without Python.
- First login and MFA, restart with persisted login, expiry and reconnect.
- Duplicate tabs, a second app holding the profile lock, browser and worker
  crashes, and app exit with browser windows open.
- Required Chromium OS libraries on the supported Linux distribution. Browser
  acquisition cannot install system packages without an OS installer.

Local Linux validation passed the Python and Rust suites, built the standalone
worker, downloaded Chromium using that executable, and launched the downloaded
browser with a temporary isolated profile. Real-service login and clean-machine
checks have not yet been performed for this change. On startup, existing profiles
are opened headlessly and checked read-only. The UI shows a checking state until
the result arrives. Clicking Log ind switches that service to a visible browser
using the same profile; a browser opened for interactive login stays visible.
No registration or approval request is made by this flow.
