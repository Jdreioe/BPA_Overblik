# Desktop validation

Issue #2 uses the same package check on Windows, macOS and Linux. The CI matrix
builds the release GUI and CLI binaries, places them beside `fixtures/`, and
runs `teamup-shift-sync-gui --self-check`. The check builds the representative
fixture preview through the core and prints its approval digest. The resulting
portable directory is uploaded as an artifact.

Dated releases wrap the same binaries in the files people download, and CI
checks each one before publishing; see [packaging](packaging.md) for what that
covers and what stays a manual check.

Run the same check locally with:

```bash
cargo build --release --workspace
./target/release/teamup-shift-sync-gui --self-check
```

## Accessibility check

The shell uses native Iced buttons with visible text, 12 to 14 pixels of
padding, text descriptions alongside every status, and no color-only meaning.
Loading, empty, failure, and setup-error states are part of the Rust widget
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

The core preview builder returns the week already translated: day labels, block
geometry in minutes from local midnight, Danish status labels, detail lines and
the attention list. The shell renders those fields and never parses summaries,
step keys, digests or outcome names. The block fill is the helper's TeamUp colour,
tinted towards white so dark text stays readable on Teamup's saturated
palette; the outline carries status. Blocks also carry a text marker (`[NY]`,
`[ÆNDRET]`, `[OK]`, `[!]`), so neither identity nor status depends on colour
alone, and the grid is repeated as text below it for screen readers.

Colour is appearance, not identity: it is kept out of the sub-calendar
comparison in `Setup.revalidate`, so recolouring a calendar in TeamUp adopts
the new colour without discarding confirmed mappings or an approval.

Frozen golden files cover the split, overnight days, DUOS hours later the same
day, changed
values, empty and unchanged weeks and an unread destination. Rust tests cover
approval requiring an unchanged re-read, a changed or newly blocked plan
returning to review, and revocation on week, setup or retry changes.

The review fixes add checks for approval when only a helper assignment, SPS
interval or Vagtmøde category changes, and for DUOS updates showing old and new
hours. Continuations into Monday keep their detailed description on the first
visible day. Simultaneous Vagtmøde helpers use separate grid lanes with the
same clock positions. A Linux/Xvfb fixture check verified two helpers at
08:00–12:00, the next shift at 12:00, and a Sunday-to-Monday shift in the text
list. No live destination writes were exercised.

Fixture mode can never enable a transfer: it reconciles no destination, so
`can_apply` stays false and the primary action stays disabled with its reason.

## Browser sessions (issue #3)

The home screen now offers separate MitHF and DUOS login buttons. On first use,
the app looks for a system Chromium or the `TEAMUP_BROWSER_PATH` executable;
it never downloads a browser. The UI opens the real service site in an
app-owned window with a separate persistent profile. Download and launch
failures offer another login attempt. Users need no terminal command.

MitHF login opens `/index.html`, the service home page. The app then clicks the
site's “Åbn din vagtplan” action itself to enter `/vagtplan/index.php`; opening that URL
directly can show MitHF's “Gå til BPA-universet” interstitial instead, so the
calendar URL is never opened directly. The app does not probe MitHF until the
visible page is inside `/vagtplan/`. The request script reads the calendar
token only from that visible page. Regression checks cover the entry URL, the
automatic entry click, the waiting state when the button is missing, and the
absence of a direct background request. Live login after this fix still needs
verification.

Each service has a separate persistent profile under `profiles/`. Login and MFA
happen in the visible service browser. A background task checks access through
read-only requests; only fixed status messages reach Iced.
The selected week and setup survive reconnecting either service. Closing the app
closes its browser contexts after pending operations finish. It never attaches to
or closes a normal browser.

Rust unit tests cover destination request building, setup validation and shape
capture; the `capture` command records the real services' JSON shapes for
checking reader assumptions. Before closing #3, manually verify on
Windows, macOS and Linux:

- First login and MFA, restart with persisted login, expiry and reconnect.
- Duplicate tabs, a second app holding the profile lock, browser crashes, and
  app exit with browser windows open.
- Required Chromium OS libraries on the supported Linux distribution. Browser
  acquisition cannot install system packages without an OS installer.

Local Linux validation passed the Rust suites and the bundle self-check. The
browser is provided by the system here, not downloaded. Real-service login and
clean-machine checks have not yet been performed for this change. On startup, existing profiles
are opened headlessly and checked read-only. The UI shows a checking state until
the result arrives. Clicking Log ind switches that service to a visible browser
using the same profile; a browser opened for interactive login stays visible.
No registration or approval request is made by this flow.
