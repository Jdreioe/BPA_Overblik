# DUOS authenticated UI observations

Inspected read-only on 2026-09-15. `Opret tidsregistrering` opens
`/usage/timeregistrations/create` (not the old screenshot route).
The form has native selects: `#Ordning`, an employee select with no ID,
and `#Type`. Inputs: `#Start`, `#Slut`, `[id="Antal timer"]`, `#Kommentar`.
The action is `Opret 1 registrering`; `Annullér` closes without saving.
No creation or approval action was clicked.

Checked again against the current frontend bundle on 2026-09-21. The citizen
form saves through `registerHours`. Acceptance is a separate action, and the
helper performs it. The app exposes only the save action, so a transfer cannot
accept a registration for the helper.

The practical-help arrangement option has value `35505`; a separate secretary
arrangement exists. The default ordinary type has value `0`. These observed
defaults still require a policy choice before live submissions.

The live employee dropdown showed:

| Source helper | DUOS display | Employee number |
| --- | --- | --- |
| Bjarne Hougaard | Bjarne Vitting Hougaard | 195071 |
| Mads Olesen | Mads Olesen | 281593 |
| Anton Skadhede | Anton Skadhede | 281549 |
| Jonas Sibast | Jonas Sibast | 281555 |
| Jeppe Heltboe | Jeppe Heltboe | 281512 |
| Ninke Hoekmann | Ninke Hoekman | 281501 |
| Zain Ahmad Alnemr | Zain Alnemr | 281550 |

Do not equate displayed employee numbers with HTML option values without
reading those values. Stable registration IDs and list completeness still need
verification with a separately authorized live batch. Existing entries are
present; an unread list must never be treated as an empty destination.

## Registration types differ per arrangement (2026-09-25)

`get-registration-types` answers per arrangement. The practical-help
arrangement `35505` offers `0` Almindelig, `1` Egen sygdom, `3`
Transporttimer, `4` Rådighed, `6` Langtidssyg, `7` Barsel med løn, `9`
Følgevagt, `22` Graviditetsbetinget sygdom, `23` Forældreorlov, `24` Barsel
uden løn, `27` Sygdom pga arbejdsskade, `32` Ferie uden løn, `38` Sygdom med
løn, `39` Graviditetsbetinget sygdom uden løn, `40` Fædreorlov med løn, `41`
Graviditetsorlov med løn, `42` Fædreorlov uden løn and `43` Graviditetsorlov
uden løn. It has no `Barn syg`; another arrangement offers it as `5`. Absence
types are therefore chosen in setup from the live list, not hard-coded.
