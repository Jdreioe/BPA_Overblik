use teamup_shift_sync_core::PlanSystem;

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
            "Åbn DUOS, og kontrollér registreringen der og i vagtplanen. Hent derefter ugen igen.",
        ),
        PlanSystem::Source | PlanSystem::Mapping => (
            "Punktet kunne ikke afgøres sikkert.",
            "Kontrollér vagten i vagtplanen, og hent ugen igen.",
        ),
    }
}

fn listed(reason: &str, system: PlanSystem) -> Option<(&'static str, &'static str)> {
    Some(match (reason, system) {
        ("uncertain_write", PlanSystem::Mithf) => ("En tidligere overførsel til MitHF nåede ikke at blive bekræftet.", "Åbn MitHF, og kontrollér om vagten blev gemt, før du overfører igen."),
        ("uncertain_write", PlanSystem::Duos) => ("En tidligere overførsel til DUOS nåede ikke at blive bekræftet.", "Åbn DUOS, og kontrollér om registreringen blev gemt, før du overfører igen."),
        ("unsupported_uni_syntax", _) => ("En SPS-linje i vagtplanen kan ikke læses.", "Ret linjen i kilden, for eksempel »uni 8-10 & 13-14«."),
        ("invalid_uni_interval", _) => ("Et SPS-tidsrum kan ikke læses.", "Skriv tidsrummet som 8-10 eller 08:00-10:00."),
        ("invalid_uni_date", _) => ("Datoen i uni-linjen kan ikke læses.", "Skriv datoen som 2026-09-15."),
        ("ambiguous_uni_date", _) => ("Vagten dækker mere end ét døgn, og uni-linjen siger ikke hvilken dag.", "Tilføj dag eller dato, for eksempel »uni 23-1 fredag«."),
        ("ambiguous_uni_weekday", _) => ("Vagten indeholder den nævnte ugedag mere end én gang.", "Skriv en dato i stedet for ugedagen."),
        ("conflicting_uni_date", _) => ("Datoen og ugedagen i uni-linjen passer ikke sammen.", "Ret den ene af dem i TeamUp."),
        ("uni_weekday_outside_shift", _) => ("Den nævnte ugedag ligger uden for vagten.", "Ret ugedagen eller vagtens tidsrum i kilden."),
        ("uni_outside_shift", _) => ("Et SPS-tidsrum ligger uden for vagten.", "Ret tidsrummet eller vagtens tidsrum i kilden."),
        ("overlapping_uni_intervals", _) => ("To SPS-tidsrum på vagten overlapper hinanden.", "Ret tidsrummene i kilden, så de ikke dækker de samme timer."),
        ("nonexistent_local_time", _) => ("Et SPS-tidsrum rammer en time, der ikke findes på grund af sommertid.", "Skriv et tidsrum uden for tidsomstillingen."),
        ("ambiguous_local_time", _) => ("Et SPS-tidsrum rammer en time, der findes to gange på grund af sommertid.", "Skriv et entydigt tidsrum i TeamUp."),
        ("no_helper_mapping", _) => ("Hjælperen fra vagtplanen er ikke koblet til en hjælper i opsætningen.", "Vælg hjælperen under Bekræft hjælpere i opsætningen."),
        ("sps_without_duos", _) => ("Vagten har SPS-timer, men DUOS er fravalgt.", "Slå DUOS til i opsætningen, eller ret SPS-feltet i vagtplanen."),
        ("missing_configuration", _) => ("DUOS-ordning og registreringstype er ikke bekræftet.", "Vælg ordning og type under Bekræft ordning i opsætningen."),
        ("destination_missing", PlanSystem::Duos) => ("Registreringen, appen overførte til DUOS, er væk.", "Er den slettet med vilje, så gør ingenting. Skal den laves igen, så vælg Tillad overførsel igen."),
        ("destination_missing", _) => ("Vagten, appen overførte til MitHF, er væk.", "Er den slettet med vilje, så gør ingenting. Skal den laves igen, så vælg Tillad overførsel igen."),
        ("manually_changed", PlanSystem::Duos) => ("Registreringen er rettet i hånden i DUOS, så appen ændrer den ikke.", "Ret DUOS eller vagtplanen, så de viser det samme, og hent ugen igen."),
        ("manually_changed", _) => ("Vagten er rettet i hånden i MitHF, så appen ændrer den ikke.", "Ret MitHF eller vagtplanen, så de viser det samme, og hent ugen igen."),
        ("not_unique", PlanSystem::Duos) => ("DUOS har flere ens registreringer for denne vagt.", "Slet dubletten i DUOS, og hent ugen igen."),
        ("not_unique", _) => ("MitHF har flere ens vagter på dette tidspunkt.", "Slet dubletten i MitHF, og hent ugen igen."),
        ("overlapping", PlanSystem::Duos) => ("DUOS har allerede timer for hjælperen i dette tidsrum.", "Ret eller slet de timer i DUOS, så de ikke overlapper, og hent ugen igen."),
        ("overlapping", _) => ("MitHF har allerede en anden vagt i dette tidsrum.", "Ret tiden på den vagt i MitHF, så den ikke overlapper, og hent ugen igen."),
        ("assigned_to_other", _) => ("Vagten i MitHF er tildelt en anden hjælper.", "Ret hjælperen i MitHF eller i vagtplanen."),
        ("existing_differs", _) => ("Vagten i MitHF har allerede andre timer end vagtplanen.", "Ret timerne i MitHF eller i vagtplanen, så de er ens, og hent ugen igen."),
        ("changed_since_sync", _) => ("SPS-timerne er ændret i MitHF efter sidste overførsel.", "Kontrollér timerne i MitHF, og ret dem ét sted."),
        ("removed_from_source", PlanSystem::Duos) => ("Timer, appen har overført til DUOS, står ikke længere i vagtplanen.", "Appen sletter aldrig selv. Slet registreringen i DUOS, hvis den skal væk."),
        ("removed_from_source", _) => ("Noget, appen har overført til MitHF, står ikke længere i vagtplanen.", "Appen sletter aldrig selv. Slet det i MitHF, hvis det skal væk."),
        ("extra_segment_removed", _) => ("En vagt blev delt op for flere SPS-tidsrum, og et af dem er væk igen.", "Slet den overflødige vagt i MitHF, hvis den ikke skal bruges."),
        ("uncertain_write", _) => ("En tidligere overførsel nåede ikke at blive bekræftet.", "Kontrollér i tjenesten, om den blev gemt, før du overfører igen."),
        ("not_pending", _) => ("DUOS-registreringen afventer ikke længere godkendelse.", "Ret den i DUOS, hvis timerne skal ændres."),
        ("rejected", _) => ("DUOS-registreringen er afvist, trukket tilbage eller modregnet.", "Kontrollér registreringen i DUOS."),
        ("offline_preview", _) => ("MitHF og DUOS er ikke aflæst i denne visning.", "Log ind i begge tjenester for at se de rigtige ændringer."),
        _ => return None,
    })
}
