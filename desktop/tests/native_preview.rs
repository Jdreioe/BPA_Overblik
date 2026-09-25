use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use teamup_shift_sync_core::{
    plan_digest, DestinationSnapshot, Outcome, PlanningConfig, SourceMarker, SourceShift, SyncPlan,
};
use teamup_shift_sync_gui::preview::build_week;

#[derive(Deserialize)]
struct Case {
    config: PlanningConfig,
    names: BTreeMap<String, String>,
    colors: BTreeMap<String, String>,
    shifts: Vec<SourceShift>,
    plan: SyncPlan,
    destination: DestinationSnapshot,
    destination_read: bool,
    expected: Value,
}

#[test]
fn native_week_matches_recorded_presentation_scenarios() {
    // Frozen at the Python removal cutover: 16 presentation scenarios.
    let cases: Vec<Case> = serde_json::from_str(include_str!("goldens/preview.json")).unwrap();
    assert!(cases.len() >= 15);
    for (index, case) in cases.into_iter().enumerate() {
        let week = build_week(
            &case.config,
            &case.names,
            &case.colors,
            &case.shifts,
            &[],
            &case.plan,
            &case.destination,
            case.destination_read,
        )
        .unwrap();
        // The recorded oracle predates targeted recovery, so compare the
        // presentation it describes. The routing fields have their own test.
        let mut presented = serde_json::to_value(&week).unwrap();
        for item in presented["attention"].as_array_mut().into_iter().flatten() {
            let item = item.as_object_mut().unwrap();
            item.remove("source_key");
            item.remove("can_allow_retransfer");
        }
        assert_eq!(presented, case.expected, "case {index}");
        let mut broken = case.plan.clone();
        if let Some(item) = broken
            .items
            .iter_mut()
            .find(|i| i.step_key == "mithf.create_shift" && i.payload.contains_key("starts_at"))
        {
            item.payload
                .insert("starts_at".into(), Value::String("not-a-time".into()));
            assert!(build_week(
                &case.config,
                &case.names,
                &case.colors,
                &case.shifts,
                &[],
                &broken,
                &case.destination,
                true
            )
            .is_err());
        }
    }
}

/// Recovery must be offered from the one conflict it can resolve, and must
/// name the exact shift it came from.
#[test]
fn only_a_missing_destination_entry_offers_allowing_a_transfer_again() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("goldens/preview.json")).unwrap();
    let mut case = cases
        .into_iter()
        .find(|case| {
            case.plan
                .items
                .iter()
                .any(|item| item.reason == "ambiguous_uni_date")
        })
        .expect("a blocked scenario");
    let week = |case: &Case| {
        build_week(
            &case.config,
            &case.names,
            &case.colors,
            &case.shifts,
            &[],
            &case.plan,
            &case.destination,
            case.destination_read,
        )
        .unwrap()
    };
    let blocked = week(&case);
    assert_eq!(blocked.attention.len(), 1);
    assert!(!blocked.attention[0].can_allow_retransfer);

    for item in &mut case.plan.items {
        if item.reason == "ambiguous_uni_date" {
            item.reason = "destination_missing".into();
        }
    }
    let missing = week(&case);
    assert_eq!(missing.attention.len(), 1);
    assert!(missing.attention[0].can_allow_retransfer);
    assert_eq!(missing.attention[0].source_key, case.shifts[0].key());
}

/// A step blocked by its shift's conflict is not listed again as its own
/// warning; the shift's conflict names the service to fix.
#[test]
fn a_shift_conflict_is_listed_once_and_names_the_service() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("goldens/preview.json")).unwrap();
    let mut case = cases
        .into_iter()
        .find(|case| {
            case.shifts.len() == 1
                && case.plan.items.iter().any(|item| {
                    item.step_key == "mithf.create_shift" && item.payload.contains_key("starts_at")
                })
                && case
                    .plan
                    .items
                    .iter()
                    .any(|item| item.step_key == "mithf.assign_helper")
        })
        .expect("a shift with an assignment");
    for item in &mut case.plan.items {
        match item.step_key.as_str() {
            "mithf.create_shift" => {
                item.outcome = Outcome::Conflicted;
                item.reason = "overlapping".into();
            }
            "mithf.assign_helper" => {
                item.outcome = Outcome::Conflicted;
                item.reason = "blocked_by_shift".into();
            }
            _ => {}
        }
    }
    let week = build_week(
        &case.config,
        &case.names,
        &case.colors,
        &case.shifts,
        &[],
        &case.plan,
        &case.destination,
        case.destination_read,
    )
    .unwrap();
    assert_eq!(week.attention.len(), 1);
    assert!(week.attention[0].explanation.contains("MitHF"));
    assert!(!week.can_apply);
}

#[test]
fn standard_shift_is_marked_in_the_week() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("goldens/preview.json")).unwrap();
    let mut case = cases
        .into_iter()
        .find(|case| {
            case.shifts.len() == 1
                && case.plan.items.iter().any(|item| {
                    item.step_key == "mithf.create_shift" && item.payload.contains_key("starts_at")
                })
        })
        .expect("a visible shift");
    case.shifts[0].standard_time = true;
    let week = build_week(
        &case.config,
        &case.names,
        &case.colors,
        &case.shifts,
        &[],
        &case.plan,
        &case.destination,
        case.destination_read,
    )
    .unwrap();
    assert!(week
        .days
        .iter()
        .flat_map(|day| &day.blocks)
        .any(|block| block.standard_time));
}

#[test]
fn day_sps_edit_on_overnight_shift_shows_old_and_new_values_in_danish() {
    let config: PlanningConfig = serde_json::from_value(json!({
        "timezone": "Europe/Copenhagen", "default_helper_count": 1,
        "duos_arrangement_id": "arrangement", "duos_registration_type": "Almindelig",
        "helpers": {"helper": {"mithf_name": "Mit Helper", "duos_employee_number": "123"}}
    }))
    .unwrap();
    let shift: SourceShift = serde_json::from_value(json!({
        "calendar_id": "calendar", "event_id": "event", "occurrence_id": "occurrence",
        "title": "Shift", "helper_key": "helper", "notes": "uni 2026-09-14 10-12",
        "starts_at": "2026-09-14T09:00:00+02:00", "ends_at": "2026-09-15T09:00:00+02:00"
    }))
    .unwrap();
    let source = shift.key();
    let destination: DestinationSnapshot = serde_json::from_value(json!({
        "mithf_shifts": [{
            "id": "7001", "starts_at": "2026-09-14T08:00:00+02:00",
            "ends_at": "2026-09-15T08:00:00+02:00", "helper_count": 1,
            "helper_name": "Mit Helper",
            "sps_intervals": [{"starts_at": "2026-09-14T10:00:00+02:00", "ends_at": "2026-09-14T11:00:00+02:00"}]
        }]
    }))
    .unwrap();
    let plan: SyncPlan = serde_json::from_value(json!({
        "starts_at": "2026-09-14T00:00:00+02:00",
        "ends_at": "2026-09-21T00:00:00+02:00",
        "generated_at": "2026-09-14T20:00:00+02:00",
        "items": [
            {"source_key": source, "system": "mithf", "step_key": "mithf.create_shift",
             "outcome": "would_update", "summary": "", "reason": "", "destination_id": "7001",
             "payload": {"starts_at": "2026-09-14T09:00:00+02:00", "ends_at": "2026-09-15T09:00:00+02:00", "helper_count": 1}},
            {"source_key": source, "system": "mithf", "step_key": "mithf.assign_helper",
             "outcome": "already_matched", "summary": "", "reason": "", "destination_id": "7001",
             "payload": {"helper_name": "Mit Helper"}},
            {"source_key": source, "system": "mithf", "step_key": "mithf.set_sps",
             "outcome": "would_update", "summary": "", "reason": "", "destination_id": "7001",
             "payload": {"intervals": [{"starts_at": "2026-09-14T10:00:00+02:00", "ends_at": "2026-09-14T12:00:00+02:00"}]}}
        ]
    }))
    .unwrap();
    let week = build_week(
        &config,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[shift],
        &[],
        &plan,
        &destination,
        true,
    )
    .unwrap();
    let block = &week.days[0].blocks[0];
    assert_eq!(block.status_label, "Ændres");
    assert!(block
        .details
        .contains(&"Vagtens tid ændres i MitHF: 08:00 14. sep – 08:00 15. sep → 09:00 14. sep – 09:00 15. sep.".into()));
    assert!(block
        .details
        .contains(&"SPS-timer ændres i MitHF: 10:00–11:00 → 10:00–12:00.".into()));
    assert!(week.can_apply);
    assert_eq!(week.days[1].blocks[0].details, block.details);

    let mut changed = plan.clone();
    changed.items[2].payload["intervals"][0]["ends_at"] = json!("2026-09-14T13:00:00+02:00");
    assert_ne!(plan_digest(&changed).unwrap(), plan_digest(&plan).unwrap());
}

/// A day-off wish over a shift is shown on its day, but it is never
/// transferred and does not hold back the week's approval.
#[test]
fn a_marker_over_a_shift_is_shown_without_blocking_approval() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("goldens/preview.json")).unwrap();
    let case = cases
        .into_iter()
        .find(|case| case.shifts.len() == 1 && case.expected["can_apply"] == json!(true))
        .expect("an approvable week with one shift");
    let shift = &case.shifts[0];
    let wish = |helper_key: &str| SourceMarker {
        helper_key: helper_key.into(),
        title: "Ønsker fri".into(),
        starts_at: shift.starts_at,
        ends_at: shift.ends_at,
        all_day: true,
    };
    let week = |markers: &[SourceMarker]| {
        build_week(
            &case.config,
            &case.names,
            &case.colors,
            &case.shifts,
            markers,
            &case.plan,
            &case.destination,
            case.destination_read,
        )
        .unwrap()
    };

    let marked = week(&[wish(&shift.helper_key)]);
    assert!(marked.can_apply);
    assert!(marked.attention.is_empty());
    let day = marked
        .days
        .iter()
        .find(|day| !day.markers.is_empty())
        .expect("a marked day");
    assert_eq!(day.markers[0].time_label, "hele dagen");
}
