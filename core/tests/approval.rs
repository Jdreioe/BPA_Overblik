use chrono::DateTime;
use serde::Deserialize;
use serde_json::json;
use teamup_shift_sync_core::{
    payload_digest, plan_digest, ApprovalError, ApprovedPlan, Outcome, PlanItem, PlanSystem,
    StepAction, SyncPlan,
};

fn plan() -> SyncPlan {
    let moment = DateTime::parse_from_rfc3339("2026-09-20T20:00:00+02:00").unwrap();
    SyncPlan {
        starts_at: moment,
        ends_at: moment,
        generated_at: moment,
        items: vec![PlanItem {
            source_key: "source".into(),
            system: PlanSystem::Mithf,
            step_key: "mithf.create_shift".into(),
            outcome: Outcome::WouldCreate,
            summary: "Create shift".into(),
            reason: String::new(),
            destination_id: None,
            payload: json!({"helper_count": 1, "starts_at": "2026-09-20T20:00:00+02:00"})
                .as_object()
                .unwrap()
                .clone(),
        }],
    }
}

#[test]
fn approval_binds_dates_order_identity_actions_and_payload_but_not_display_text() {
    let original = plan();
    let digest = plan_digest(&original).unwrap();
    let mut presentation = original.clone();
    presentation.generated_at += chrono::Duration::minutes(1);
    presentation.items[0].summary = "Translated text".into();
    presentation.items[0].reason = "translated_reason".into();
    assert_eq!(plan_digest(&presentation).unwrap(), digest);
    let mutations: Vec<fn(&mut SyncPlan)> = vec![
        |p| p.starts_at += chrono::Duration::seconds(1),
        |p| p.ends_at += chrono::Duration::seconds(1),
        |p| p.items[0].source_key = "other".into(),
        |p| p.items[0].step_key = "mithf.create_shift#1".into(),
        |p| p.items[0].outcome = Outcome::WouldUpdate,
        |p| p.items[0].destination_id = Some("new-id".into()),
        |p| {
            p.items[0].payload.insert("helper_count".into(), json!(2));
        },
    ];
    for mutate in mutations {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert_eq!(
            ApprovedPlan::validate(&changed, &digest).unwrap_err(),
            ApprovalError::PlanChanged
        );
    }
    let mut ordered = original.clone();
    ordered.items.push(PlanItem {
        step_key: "mithf.assign_helper".into(),
        ..original.items[0].clone()
    });
    let digest = plan_digest(&ordered).unwrap();
    ordered.items.reverse();
    assert_ne!(plan_digest(&ordered).unwrap(), digest);
    let mut unsupported = original;
    unsupported.items[0]
        .payload
        .insert("hours".into(), json!(1.5));
    assert_eq!(
        plan_digest(&unsupported),
        Err(ApprovalError::UnsupportedNumber)
    );
}

#[test]
fn blockers_and_shared_destination_claims_stop_the_batch() {
    for outcome in [
        Outcome::Conflicted,
        Outcome::Review,
        Outcome::Failed,
        Outcome::PendingIntegration,
    ] {
        let mut plan = plan();
        plan.items[0].outcome = outcome;
        assert_eq!(
            ApprovedPlan::validate(&plan, &plan_digest(&plan).unwrap()).unwrap_err(),
            ApprovalError::UnresolvedItems
        );
    }
    let mut plan = plan();
    plan.items[0].destination_id = Some("shared".into());
    plan.items.push(PlanItem {
        source_key: "other-source".into(),
        step_key: "mithf.create_shift#1".into(),
        ..plan.items[0].clone()
    });
    assert_eq!(
        ApprovedPlan::validate(&plan, &plan_digest(&plan).unwrap()).unwrap_err(),
        ApprovalError::SharedDestination
    );
    plan.items[1].destination_id = Some("other-destination".into());
    assert!(ApprovedPlan::validate(&plan, &plan_digest(&plan).unwrap()).is_ok());
}

#[test]
fn step_checks_allow_progress_and_stop_changed_or_unapproved_writes() {
    let original = plan();
    let approved = ApprovedPlan::validate(&original, &plan_digest(&original).unwrap()).unwrap();
    let mut current = original.clone();
    current.items[0].destination_id = Some("new-shift".into());
    current.items[0].outcome = Outcome::WouldUpdate;
    assert!(matches!(
        approved.check_step(0, &current),
        Ok(StepAction::Write(_))
    ));
    assert_eq!(
        approved.check_read_back(0, &current),
        Err(ApprovalError::VerificationFailed)
    );
    assert_eq!(
        approved.check_final(&current),
        Err(ApprovalError::FinalUnresolved)
    );
    current.items[0].outcome = Outcome::AlreadyMatched;
    assert!(matches!(
        approved.check_step(0, &current),
        Ok(StepAction::AlreadyMatched(_))
    ));
    assert!(approved.check_read_back(0, &current).is_ok());
    assert!(approved.check_final(&current).is_ok());

    let matched_approval =
        ApprovedPlan::validate(&current, &plan_digest(&current).unwrap()).unwrap();
    assert_eq!(
        matched_approval.check_step(0, &original),
        Err(ApprovalError::UnapprovedWrite)
    );
    current.items[0]
        .payload
        .insert("helper_count".into(), json!(2));
    assert_eq!(
        approved.check_step(0, &current),
        Err(ApprovalError::DestinationChanged)
    );
    current.items.clear();
    assert_eq!(
        approved.check_step(0, &current),
        Err(ApprovalError::DestinationChanged)
    );
    assert_eq!(
        approved.check_read_back(0, &current),
        Err(ApprovalError::VerificationFailed)
    );
    assert_eq!(
        approved.check_step(1, &current),
        Err(ApprovalError::UnknownStep)
    );

    for outcome in [
        Outcome::Conflicted,
        Outcome::Review,
        Outcome::Failed,
        Outcome::PendingIntegration,
        Outcome::Excluded,
    ] {
        let mut changed = original.clone();
        changed.items[0].outcome = outcome;
        let expected = if outcome == Outcome::Excluded {
            ApprovalError::UnapprovedWrite
        } else {
            ApprovalError::DestinationChanged
        };
        assert_eq!(approved.check_step(0, &changed), Err(expected));
    }
    let mut excluded = original;
    excluded.items[0].outcome = Outcome::Excluded;
    let approved = ApprovedPlan::validate(&excluded, &plan_digest(&excluded).unwrap()).unwrap();
    assert_eq!(approved.check_step(0, &current), Ok(StepAction::Skip));
}

#[derive(Deserialize)]
struct Oracle {
    traces: Vec<Trace>,
    hash_case: HashCase,
}
#[derive(Deserialize)]
struct HashCase {
    plan: SyncPlan,
    digest: String,
    payload_hash: String,
}
#[derive(Deserialize)]
struct Trace {
    digest: String,
    plans: Vec<SyncPlan>,
    error: Option<String>,
}

#[test]
fn digests_and_validation_match_recorded_apply_runs() {
    // Frozen at the Python removal cutover: 10 recorded apply traces.
    let oracle: Oracle = serde_json::from_str(include_str!("goldens/approval.json")).unwrap();
    assert_eq!(
        plan_digest(&oracle.hash_case.plan).unwrap(),
        oracle.hash_case.digest
    );
    assert_eq!(
        payload_digest(&oracle.hash_case.plan.items[0]).unwrap(),
        oracle.hash_case.payload_hash
    );
    assert!(oracle.traces.len() >= 6);
    for trace in oracle.traces {
        let mut plans = trace.plans.iter();
        let initial = plans.next().unwrap();
        let approved = match ApprovedPlan::validate(initial, &trace.digest) {
            Ok(approved) => approved,
            Err(error) => {
                assert_eq!(trace.error.as_deref(), Some(error.to_string().as_str()));
                assert!(plans.next().is_none());
                continue;
            }
        };
        let mut interrupted = false;
        for (index, original) in approved.plan().items.iter().enumerate() {
            if original.outcome == Outcome::Excluded {
                continue;
            }
            match approved.check_step(index, plans.next().unwrap()).unwrap() {
                StepAction::AlreadyMatched(_) => {}
                StepAction::Write(_) => {
                    // The recorded integration traces deliberately lose a write
                    // response. No read-back exists; Rust must not verify it.
                    if let Some(read_back) = plans.next() {
                        approved.check_read_back(index, read_back).unwrap();
                    } else {
                        assert_eq!(trace.error.as_deref(), Some("Response lost"));
                        interrupted = true;
                        break;
                    }
                }
                StepAction::Skip => unreachable!(),
            }
        }
        if !interrupted {
            approved.check_final(plans.next().unwrap()).unwrap();
            assert!(trace.error.is_none());
        }
        assert!(plans.next().is_none());
    }
}
