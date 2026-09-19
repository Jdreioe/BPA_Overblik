# Guided desktop setup

Issue #4 adds Danish setup to the Iced app. Normal launches use live services;
example data requires an explicit `TEAMUP_FIXTURE` environment variable.

## First run

1. Paste a shared `https://teamup.com/ks…` calendar link and the one-time API
   key. The app reads calendar configuration and the current week's events,
   including each event's comments. Account-only, password-protected and
   restricted links need a shared link with suitable access from the calendar
   owner. Empty weeks cannot prove comment visibility; later previews check it.
2. Sign in to MitHF and DUOS in their app-managed browsers. Fetch helpers and
   arrangements after login completes.
3. Choose an active DUOS SPS arrangement and registration type. A unique choice
   is a proposal requiring confirmation. Multiple MitHF customers or selectable
   grants block setup because the existing adapter does not validate arbitrary
   customer/grant combinations.
4. Review TeamUp, MitHF and DUOS names together. Unique exact names are proposed;
   other matches require dropdown selections. Exclude calendars that are not
   helper calendars. DUOS duplicate names have identifying numbers in the
   dropdown; users select an existing entry and never type an employee number.
   MitHF duplicate names block confirmation because its current assignment
   read-back cannot distinguish them safely.
5. Confirm the customer, grant, arrangement, registration type and helpers.
   The app re-reads discovery before saving confirmation and opens this week's
   live preview. This flow does not create or approve registrations.

Closing the app preserves submitted credentials, discovery and each dropdown
or exclusion edit. Failed discovery can be retried. Existing local TOML files
are offered for import using the desktop's existing config-path resolution.
Imports become proposals for review; the original file is not changed. Legacy
configurations using fields other than helper subcalendars need fresh setup.

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

The key, capability link and any imported bearer token go into Windows
Credential Locker, macOS Keychain or Linux Secret Service through
[keyring](https://keyring.readthedocs.io/en/latest/). The app explicitly selects
these OS backends; it never substitutes a plaintext or null backend. Linux
requires an unlocked Secret Service in the user's desktop session. A missing
or locked store produces a retryable setup error.

The app data directory contains an atomically replaced `setup.json` with names,
stable IDs, exclusions, arrangement choices and an opaque credential reference.
On Unix, its directory is mode 0700 and the file is 0600. Windows uses the
current user's application-data directory and inherited user ACLs. Browser
profiles remain inside that protected directory. Setup values never go into
the repository. The original imported TOML can still contain old secrets;
the import notice makes that explicit without deleting user data.

Before confirmation and every live preview, the app checks calendars, helper
IDs and names, active employment, the selected arrangement and type, and the
MitHF customer/grant. Changes require renewed confirmation. Each confirmed
account/mapping combination uses a separate SQLite state filename. Credentials,
response bodies, names and URLs are omitted from worker error diagnostics.

## Validation

`tests/test_setup.py` covers resume, restricted comments, invalid links,
ambiguous helpers, inactive employment, absent SPS arrangements, multiple
accounts and arrangements, import, changed identities and secret redaction.
Rust tests cover rendering and recovery transitions. The live preview reuses
the existing planner and read-only destination adapters.

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
