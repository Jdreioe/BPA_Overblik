# DUOS authenticated UI observations

Inspected read-only on 2026-09-15. `Opret tidsregistrering` opens
`/usage/timeregistrations/create` (not the old screenshot route).
The form has native selects: `#Ordning`, an employee select with no ID,
and `#Type`. Inputs: `#Start`, `#Slut`, `[id="Antal timer"]`, `#Kommentar`.
The action is `Opret 1 registrering`; `Annullér` closes without saving.
No creation or approval action was clicked.

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
reading those values. Date picker selection and full year read-back, stable
registration IDs, list completeness, and save-versus-approval semantics remain
to be implemented and verified. Existing entries are present; an unread list
must never be treated as an empty destination.
