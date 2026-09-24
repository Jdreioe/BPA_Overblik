# Guided desktop setup

Issue #4 adds Danish setup to the Iced app. Normal launches use live services.
Fixture previews are CLI-only; the desktop app never opens example data.

## First run

1. Choose TeamUp or **Regneark** (a spreadsheet). For TeamUp, paste a shared `https://teamup.com/ks…` calendar link and the one-time API
   key. The app reads calendar configuration and the current week's events,
   including each event's comments. Account-only, password-protected and
   restricted links need a shared link with suitable access from the calendar
   owner. Empty weeks cannot prove comment visibility; later previews check it.
2. Under **Indstillinger → Udbydere**, sign in to MitHF under **Løn**, and, if SPS registration is enabled, to DUOS in its own pane. Each pane has **Log ind**, **Check forbindelse** and **Log ud**.
   The app fetches helpers and arrangements on the first **Hjælpere** visit after the connections are ready. Use its round refresh icon to fetch them again.
3. If DUOS is enabled, choose an active SPS arrangement. The registration type
   is always the ordinary one. Multiple MitHF customers or selectable
   grants block setup because the existing adapter does not validate arbitrary
   customer/grant combinations.
4. Review source, MitHF and, when enabled, DUOS names together. Unique exact names are proposed;
   other matches require dropdown selections. Exclude calendars that are not
   helper calendars. DUOS duplicate names have identifying numbers in the
   dropdown; users select an existing entry and never type an employee number.
   MitHF duplicate names block confirmation because its current assignment
   read-back cannot distinguish them safely.
5. Choices and exclusions save as they change. When the mappings are valid, the
   app re-reads the services and activates the setup automatically. Review the
   week before any transfer. This flow does not create or approve registrations.

Closing the app preserves submitted credentials, discovery and each dropdown
or exclusion edit. Failed discovery can be retried. **Opdatér hjælpere** after
adding a helper to the source keeps every unchanged helper's choices; only a
new or renamed helper is proposed again. A changed MitHF or DUOS catalog still
proposes every mapping again.


## Spreadsheet source

Choose **Regneark**, then paste a link or choose **Vælg fil …**. The app only
reads the spreadsheet, never edits it, and needs no account or API key.

| Source | What to paste or choose |
| --- | --- |
| Google Sheets | The tab's `docs.google.com/spreadsheets/d/…/edit` link, shared as **Anyone with the link can view**. The app reads that tab's CSV export. |
| Google Sheets, published | A **File → Share → Publish to web** link. `pubhtml` is read as the whole workbook. |
| OneDrive, Excel for the web | A view-only **Anyone with the link** share link, including `1drv.ms` short links. |
| SharePoint | A view-only **Anyone with the link** share link. |
| Nextcloud, ownCloud | A public share link (`…/s/<token>`). |
| Dropbox | A share link. |
| Anything else | An `https://` link that downloads an `.xlsx`, `.ods` or `.csv` file itself. |
| A file on this computer | **Vælg fil …**. This also covers a file in a synced OneDrive, Dropbox or iCloud Drive folder. |

The app rewrites a known provider's share link to its download address. It
fetches only `https://`, redirects included, and refuses files over 10 MB or
downloads over 30 seconds. A link that returns a login page instead of a file
says so. A chosen file is read again on every preview and before every
transfer, just like a link.

The file's type is judged by its content, not its name:

- **CSV:** separated by `,` or by `;` (as Danish Excel saves it), with or
  without a UTF-8 byte order mark.
- **`.xlsx` and `.ods`:** each cell is read as the text the person sees, which
  is what the template below is learned from. An `.ods` file stores that text.
  An `.xlsx` file stores values and number formats, so dates, times and numbers
  are shown the way Danish Excel and LibreOffice show them: `02-11-2026`,
  `mandag`, `08:00`, `7,5`. A merged range keeps its value in the top-left cell.
  Formulas use their saved result and are never recalculated.
- **`.xls` and `.xlsb`:** refused with a request to save as `.xlsx`. Their number
  formats can't be read, so a date cell that shows only a weekday or a month
  would be read as a date.
- **Apple Numbers:** not supported, and neither are iCloud share links. Export
  to `.xlsx`, or keep the file in iCloud Drive and choose it as a local file.

A workbook with several tabs asks which one to read, and the choice is kept by
name. A renamed or removed tab gives a message instead of reading another tab.
Choose it again under **Indstillinger → Udbydere**.

The link or path and the tab are stored in the OS credential store, never in
`setup.json` or a diagnostics report. A share link grants access, and a path
can contain the user's name. Sync history belongs to the link or path and the
tab, so a Google Sheets setup keeps its history. Moving the same plan to
another provider, file or tab starts a new history. Its earlier transfers are
then not recognised, so check the first week carefully.

Setup is three steps: **1. Hvor ligger dit regneark?** (a link or a file),
**2. Hvordan ser en vagt ud for dig?**, and **3. Tilslut**. Each step appears
once the one before is done. In step 2, copy the cells of one filled shift
(for a weekly grid, one day's column: date, weekday, helper, time, SPS) and
choose **Indsæt vagt**. The app then asks one question at a time, such as
"Hvor står datoen?", answered by clicking the cell. SPS hours and a title such
as `P-MØDE` can be answered with **Ingen SPS** or **Ingen titel**. A sheet
that holds only the helper answers the time with **Ingen tid – brug
standardtid**, and every shift takes the standard time. If the time cell holds
only a start time, the app also asks for the end time. **Start
forfra** goes back to pasting. A workbook with several tabs asks for one in
step 3.
The app learns the date format and the word before SPS hours (such as `Sps`)
from the example, and checks that the example reads as one shift. Only the
cells' positions relative to the date are saved, never their contents.

When reading, every cell holding a date in that format anchors one shift, so
the same template covers a weekly grid and a table with one shift per row.
A date with nothing at the template's positions, such as a heading, is
ignored. A date written without a year, such as `23/9`, takes the year nearest
the week being read, so `3/1` in the week after Christmas is next January. The
year may only be left out with `/`, because `23.9` and `23-9` look like times. An impossible date such as `31/11/26` is reported, not skipped. One
shift per date is supported.

The [ugenr.dk](https://ugenr.dk/kalender) year calendar shows a day as its
weekday's initial and day of the month, such as `F  2`, below a month heading
such as `Januar 2026`. A template pasted from it learns that format, and each
day then takes its month and year from the nearest heading above it. A day
that does not match its weekday, such as `M  3` in February 2026, is reported
as an impossible date. Only a template learned from such a day reads them. The
holiday ugenr.dk writes next to a day, such as `Juleaften`, is not a helper:
`Juleaften Alex` reads as `Alex`, and a holiday alone is no shift.

Setup needs at least one filled shift to discover helper names. Each source
helper must be mapped to a MitHF helper before confirmation. An unreadable
cell, or a date that appears twice, blocks every week it can touch; other weeks
still work, and setup skips it. The message names the helper and date, such as
"Zains vagt d. 27/9 mangler tid.", and only an impossible date points at its
cell. These messages are shown on screen only and never go into a report.

## Standard shift time

Under **Indstillinger → Standardtider**, enter a common time such as `6-22` for days where a
shift has no hours of its own. Leave a weekday field empty to inherit that
time, enter another range to override it, or enter `ingen` to clear it for that
weekday. The common field may be empty if only some weekdays need a standard.
Ranges accept `8-24`, `08:30-16:00`, and overnight hours such as `22-8`.
The app uses Copenhagen local time and refuses ambiguous or nonexistent times
at daylight-saving changes.

A TeamUp all-day event on exactly one date and a confirmed helper calendar uses
that date's standard. A multi-day all-day event needs its own times before the
week can be transferred. A Sheets shift with a helper and no time uses the
standard; when start and end are separate cells, both must be empty. A weekday
without a standard is reported with the shift date. The week preview marks
every such shift `standardtid`. The ordinary reminder, marker, meeting and SPS
rules still apply, so an all-day `Ønsker fri` is a marker, not a standard shift. Valid times save automatically, and a changed standard requires a
fresh week review.
A shift's identity is its date. Editing cells in place updates the same
shift. Moving a shift to another date is treated like deleting and
recreating a TeamUp event: the new position is a new shift, and the app never
removes the old one from MitHF or DUOS. The source is read afresh for preview
and again before transfer.

Disable DUOS in setup when SPS registration is not used. No DUOS login or
helper mapping is then required. If a sheet still contains SPS instructions,
the preview flags them for review before transfer.

## Switching source

Each source keeps its own setup. Choosing the other source under **Skift
vagtplan** keeps the current setup, including its link or key and helper
mappings, and choosing it again later restores it exactly. A source chosen for
the first time starts from the connection step. Each source keeps its own sync
history, because the history file is named after the source and its mappings.

## Teamup API key provisioning

Checked on 19 September 2026 against Teamup Calendar's
[official API guidance](https://calendar.teamup.com/kb/api-teamup-calendar/) and
[official Postman examples](https://www.postman.com/teamup-calendar/teamup-calendar-public-workspace/documentation/9dxuvwb/teamup-calendar-api-examples).
Both direct developers to [request an API key](https://teamup.com/api-keys/request).
The examples describe the key as secret and send it in `Teamup-Token`.

The published guidance does not establish a supported way to embed a shared
maintainer key in a public desktop installer or provision keys without the
request form. We therefore retain a guided one-time key field. The maintainer
can help a user complete the official request and enter their own issued key.
A shared distribution agreement would need confirmation from Teamup before
changing this route. No key is included in source, fixtures or installers.
This is the precise exception to calendar-link-only setup.

## Local storage and revalidation

The TeamUp key, source capability link and any imported bearer token go into the OS
credential store: Windows Credential Locker, macOS Keychain or Linux Secret
Service. The app explicitly selects these OS backends; it never substitutes a
plaintext or null backend. Linux requires an unlocked Secret Service in the
user's desktop session. A missing or locked store produces a retryable setup
error.

The app data directory contains an atomically replaced `setup.json` with names,
stable IDs, exclusions, arrangement choices and an opaque credential reference.
On Unix, its directory is mode 0700 and the file is 0600. Windows uses the
current user's application-data directory and inherited user ACLs. Browser
profiles remain inside that protected directory. Setup values never go into
the repository.

Before activating changed mappings and every live preview, the app checks calendars, helper
IDs and names, active employment, the selected arrangement and type, and the
MitHF customer/grant. Changes require a fresh check. Each confirmed
account/mapping combination uses a separate SQLite state filename. Credentials,
response bodies, names and URLs are omitted from error diagnostics.

## Validation

Recorded setup scenarios pin the state machine: refreshes that preserve or
reset mappings, renamed calendars, a changed destination catalog, automatic
selection of a single arrangement, ambiguous MitHF names and half-reviewed
confirmations, with the exact Danish error text after each step. Rust UI tests
cover rendering and recovery transitions. The live preview reuses the existing
planner and read-only destination adapters.

DUOS field names and employment eligibility were also checked against its
[public frontend bundle](https://mit.duos.dk/assets/Roster-BZBvn4Xb.js) on
19 September 2026. Arrangement labels include the service's suffix. Employment
is eligible when DUOS marks it active or its start/end dates cover the selected
day, matching the service's registration form.

Real account discovery, native keychain prompts and packaged first-run behavior
still need interactive acceptance on Windows, macOS and Linux. Fake service
responses validate logic but cannot prove the current private destination API's
MitHF readable-label fields or actual account permissions. No live registration writes
are part of this validation.
