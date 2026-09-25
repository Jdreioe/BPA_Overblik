# MitHF UI observations

Observed from the four user-supplied screenshots `3244.png`–`3247.png` on
2026-09-15. These notes describe visible behavior; screenshots do not expose
DOM attributes, so they are not a selector contract.

## Verified flow

1. **Min vagtplan** has `Liste`, `Uge`, `Skema`, `Måned`, and `Mangler` views,
   previous/next week controls, `Vælg uge`, per-day `+`, and `Ny vagt` actions.
2. **Ny vagt** has independent start and end date/time controls. The supplied
   form showed an 08:00–16:00 shift and calculated `Varighed: 8 timer`. It also
   offers `Lås vagtlængde` and preset durations, including an overnight/24-hour
   preset. Automation must set and then read back both endpoints and duration;
   it must not rely on duration locking.
3. The helper-count control defaults to one in the screenshot. Category buttons
   shown are `Akut tillæg`, `Delt tjeneste`, `Vagtmøde`, `MUS/APV`, `SPS timer`,
   and `Oplæring`. The UI states: `Typetimer sættes på hele vagten. Du kan altid
   rette tidsrummet bagefter.`
4. The ordinary creation action is `Læg vagten ind, og vælg hjælper`. The
   separate sickness action is not part of imports.
5. **Hvem tager den?** contains a name search and helper list. The screenshot
   clearly shows Anton Skadhede, Bjarne Hougaard, Jeppe Heltboe, Jonas Sibast,
   Mads Olesen, and Zain Alnemr.
6. The helper picker simultaneously displays `Vagten er lagt ind ✓`. Therefore
   creation has succeeded before assignment. The state machine must checkpoint
   the created shift, then resume assignment against that shift after a crash.

## Authenticated DOM inspection (2026-09-15)

Read-only inspection confirmed `#fab` opens creation, `#bLuk` closes the
`#ark` dialog, and list shift buttons expose stable `data-id` and `data-dato`.
Creation fields are `#tidStart`, `#tidSlut`, `#slutDatoNy`, `#antalNy`, and
`#laasLaeng`. Start-day navigation uses `data-hop`; end-day uses `data-shop`.
`#bGem` creates a shift: it was not clicked.

Existing shift detail exposes SPS records on `[data-tt="4:5"]`, including
`data-fjern` (record ID), `data-fd`/`data-td` (DD.MM.YYYY dates) and
`data-ff`/`data-tf` (times). `[data-ret="<record-id>"]` opens interval editing;
`#rF_<record-id>` and `#rT_<record-id>` hold start/end. Opening and cancelling
the editor did not save anything. Category toggles can write immediately;
do not click one merely to inspect it.

List badges show full shift duration, not SPS duration. Use the interval in
shift detail. List view omitted a carry-in shift visible in week view;
list-only reconciliation cannot establish complete range coverage. Closed
dialog contents remain in the DOM: do not treat their presence as open state.

## Still requires verification

On 2026-09-23, Jonas reported a successful live time edit on a 24-hour shift
and a successful SPS edit on a split shift. The exact read-back status is not
yet confirmed. Overnight SPS intervals are outside the intended use.

- The confirmation state after choosing a helper and the read-back path for the
  saved assignment.
- `rettid` save/read-back for an ordinary shift and an explicit `AlreadyMatched`
  read-back for the reported 24-hour edit.
- `retreg` read-back with an existing SPS record ID.
- SPS read-back on a shift created by splitting. The planner cuts a shift at
  the start of each further SPS interval so every MitHF shift carries exactly
  one. The split edit was reported to save successfully, but its read-back
  outcome has not yet been confirmed.

No live shift should be created merely to discover these details. Use an
already authorized batch or explicit permission for a harmless concrete test.

The shift plan is the source of truth. The weekly flow proposes time, SPS and
Vagtmøde edits whenever MitHF differs from it, including changes made by hand
in MitHF, and adopts a hand-entered shift with the same helper or none. It
shows the old and new values before approval and requires matching read-back
after each edit. Deleting shifts or SPS records, replacing a booked helper and
changing the helper count are not supported actions, so those differences stay
warnings for the user to fix in MitHF.
