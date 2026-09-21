use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use teamup_shift_sync_core::{DestinationSnapshot, PlanningConfig, SourceShift, SyncPlan};
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
