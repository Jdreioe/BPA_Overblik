use teamup_shift_sync_core::PlanSystem;

/// DUOS takes no edit from the app beyond a pending registration, so every
/// DUOS problem is left for the user to settle on DUOS itself.
pub const DUOS_ACTION: &str = "Tjek vagten på mit.duos.dk, og registrér timerne selv.";

// Danish explanations ported from the existing week preview. `system` names
// the service to check when the outcome is unresolved.
pub(super) fn explanation(reason: &str, system: PlanSystem) -> (&'static str, &'static str) {
    if let Some(known) = listed(reason, system) {
        return known;
    }
    match system {
        PlanSystem::Mithf => (
            "Vagten kunne ikke afgøres sikkert i MitHF.",
            "Åbn MitHF, og kontrollér vagten der og i vagtplanen. Hent derefter ugen igen.",
        ),
        PlanSystem::Duos => (
            "Registreringen kunne ikke afgøres sikkert i DUOS.",
            DUOS_ACTION,
        ),
        PlanSystem::Source | PlanSystem::Mapping => (
            "Punktet kunne ikke afgøres sikkert.",
            "Kontrollér vagten i vagtplanen, og hent ugen igen.",
        ),
    }
}

fn listed(reason: &str, system: PlanSystem) -> Option<(&'static str, &'static str)> {
    Some(match (reason, system) {
        ("unsupported_uni_syntax", _) => (
            "En SPS-linje i vagtplanen kan ikke læses.",
            "Ret linjen i kilden, for eksempel »uni 8-10 & 13-14«.",
        ),
        ("invalid_uni_interval", _) => (
            "Et SPS-tidsrum kan ikke læses.",
            "Skriv tidsrummet som 8-10 eller 08:00-10:00.",
        ),
        ("invalid_uni_date", _) => (
            "Datoen i uni-linjen kan ikke læses.",
            "Skriv datoen som 2026-09-15.",
        ),
        ("ambiguous_uni_date", _) => (
            "Vagten dækker mere end ét døgn, og uni-linjen siger ikke hvilken dag.",
            "Tilføj dag eller dato, for eksempel »uni 23-1 fredag«.",
        ),
        ("ambiguous_uni_weekday", _) => (
            "Vagten indeholder den nævnte ugedag mere end én gang.",
            "Skriv en dato i stedet for ugedagen.",
        ),
        ("conflicting_uni_date", _) => (
            "Datoen og ugedagen i uni-linjen passer ikke sammen.",
            "Ret den ene af dem i TeamUp.",
        ),
        ("uni_weekday_outside_shift", _) => (
            "Den nævnte ugedag ligger uden for vagten.",
            "Ret ugedagen eller vagtens tidsrum i kilden.",
        ),
        ("uni_outside_shift", _) => (
            "Et SPS-tidsrum ligger uden for vagten.",
            "Ret tidsrummet eller vagtens tidsrum i kilden.",
        ),
        ("overlapping_uni_intervals", _) => (
            "To SPS-tidsrum på vagten overlapper hinanden.",
            "Ret tidsrummene i kilden, så de ikke dækker de samme timer.",
        ),
        ("nonexistent_local_time", _) => (
            "Et SPS-tidsrum rammer en time, der ikke findes på grund af sommertid.",
            "Skriv et tidsrum uden for tidsomstillingen.",
        ),
        ("ambiguous_local_time", _) => (
            "Et SPS-tidsrum rammer en time, der findes to gange på grund af sommertid.",
            "Skriv et entydigt tidsrum i TeamUp.",
        ),
        ("unreadable_absence", _) => (
            "En fraværslinje i vagtplanen kan ikke læses.",
            "Skriv linjen som »SYG: Anna« eller »SYG 8-12: Anna«.",
        ),
        ("ambiguous_absence_date", _) => (
            "Vagten dækker mere end ét døgn, og fraværslinjen siger ikke hvilken dag.",
            "Skriv datoen foran tiden, for eksempel »SYG 2026-09-28 8-12: Anna«.",
        ),
        ("absence_outside_shift", _) => (
            "Et fraværstidsrum ligger uden for vagten.",
            "Ret tidsrummet i fraværslinjen eller vagtens tidsrum i kilden.",
        ),
        ("overlapping_absences", _) => (
            "To fraværslinjer på vagten dækker de samme timer.",
            "Ret linjerne, så hver time kun står én gang.",
        ),
        ("unknown_absence_helper", _) => (
            "Hjælperen i fraværslinjen findes ikke blandt de koblede hjælpere.",
            "Skriv hjælperens navn, som det står i vagtplanen, eller kobl hjælperen under Hjælpere.",
        ),
        ("ambiguous_absence_helper", _) => (
            "Flere hjælpere har det fornavn, der står i fraværslinjen.",
            "Skriv hele navnet i fraværslinjen.",
        ),
        ("absence_helper_is_planned", _) => (
            "Fraværslinjen nævner vagtens egen hjælper.",
            "Skriv navnet på den, der tog vagten.",
        ),
        ("sps_crosses_absence", _) => (
            "Et SPS-tidsrum ligger delvist i et fravær.",
            "Del SPS-tidsrummet ved fraværets start eller slut, så det ligger hos én hjælper.",
        ),
        ("absence_duos_type", _) => (
            "Der er SPS-timer i et fravær, men ikke valgt, hvordan DUOS skal registrere dem.",
            "Vælg Åbn indstillingen, og vælg en DUOS-type under Fravær.",
        ),
        ("absent_in_mithf", _) => (
            "MitHF har hjælperen meldt syg, men vagtplanen har ingen fraværslinje.",
            "Skriv fx »SYG: navn« på vagten, eller fortryd sygemeldingen i MitHF.",
        ),
        ("absence_removed", _) => (
            "Fraværet står ikke længere i vagtplanen, men hjælperen er stadig meldt syg i MitHF.",
            "Fortryd sygemeldingen i MitHF, og hent ugen igen. Appen fortryder aldrig selv.",
        ),
        ("absence_reason_changed", _) => (
            "Fraværsårsagen er ændret, efter hjælperen blev meldt syg i MitHF.",
            "Ret årsagen i MitHF, eller skriv den gamle årsag i vagtplanen.",
        ),
        ("sps_on_absent_shift", _) => (
            "SPS-timerne står stadig på den fraværende hjælpers vagt i MitHF.",
            "Fjern SPS-timerne fra den vagt i MitHF, og hent ugen igen. Appen sætter dem på afløserens vagt.",
        ),
        ("no_helper_mapping", _) => (
            "Hjælperen fra vagtplanen er ikke koblet til en hjælper i MitHF.",
            "Vælg Åbn indstillingen, og kobl hjælperen under Hjælpere.",
        ),
        ("sps_without_duos", _) => (
            "Vagten har SPS-timer, men DUOS er slået fra.",
            "Vælg Åbn indstillingen, og slå DUOS til. Er timerne ikke SPS, så ret vagtplanen.",
        ),
        ("missing_configuration", _) => (
            "DUOS-ordning og registreringstype er ikke valgt.",
            "Vælg Åbn indstillingen, og vælg ordning og type under DUOS.",
        ),
        ("not_unique", PlanSystem::Duos) => (
            "DUOS har flere ens registreringer for denne vagt.",
            DUOS_ACTION,
        ),
        ("overlapping", PlanSystem::Duos) => (
            "DUOS har allerede andre timer for hjælperen i dette tidsrum.",
            DUOS_ACTION,
        ),
        ("removed_from_source", PlanSystem::Duos) => {
            ("Timer i DUOS står ikke længere i vagtplanen.", DUOS_ACTION)
        }
        ("not_pending", _) => (
            "DUOS-registreringen afventer ikke længere, så appen kan ikke rette den.",
            DUOS_ACTION,
        ),
        ("rejected", _) => (
            "DUOS-registreringen er afvist, trukket tilbage eller modregnet.",
            DUOS_ACTION,
        ),
        ("not_unique", _) => (
            "MitHF har flere ens vagter på dette tidspunkt.",
            "Slet de ekstra vagter i MitHF, og hent ugen igen.",
        ),
        ("overlapping", _) => (
            "MitHF har en vagt i dette tidsrum, som ikke hører til vagtplanen.",
            "Slet eller flyt den vagt i MitHF, og hent ugen igen.",
        ),
        ("assigned_to_other", _) => (
            "Vagten i MitHF er booket til en anden hjælper.",
            "Fjern den anden hjælper fra vagten i MitHF, og hent ugen igen.",
        ),
        ("helper_count", _) => (
            "Vagten i MitHF er sat til flere hjælpere, og det kan appen ikke rette.",
            "Sæt vagten til én hjælper i MitHF, og hent ugen igen.",
        ),
        ("extra_intervals", _) => (
            "Vagten i MitHF har flere SPS- eller mødetidsrum, end vagtplanen har.",
            "Slet de ekstra tidsrum på vagten i MitHF, og hent ugen igen.",
        ),
        ("removed_from_source", _) => (
            "Noget i MitHF står ikke længere i vagtplanen.",
            "Slet det i MitHF, hvis det skal væk. Appen sletter aldrig selv.",
        ),
        ("extra_segment_removed", _) => (
            "Vagtplanen har færre SPS-tidsrum end før, så en del af vagten i MitHF er til overs.",
            "Slet den ekstra vagt i MitHF, og hent ugen igen.",
        ),
        ("offline_preview", _) => (
            "MitHF og DUOS er ikke aflæst i denne visning.",
            "Log ind i begge tjenester for at se de rigtige ændringer.",
        ),
        _ => return None,
    })
}
