//! Exact-plan approval and read-only checks for transfer orchestration.
//!
//! Callers must hold the apply lock and rebuild plans from complete destination
//! reads. These checks do not submit writes or persist recovery markers.
use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{state::isoformat, Outcome, PlanItem, PlanSystem, SyncPlan};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApprovalError {
    #[error("Approval payload contains a non-integer number")]
    UnsupportedNumber,
    #[error("The plan changed. Review a fresh dry-run before applying it")]
    PlanChanged,
    #[error("The selected batch has unresolved items; no writes were made")]
    UnresolvedItems,
    #[error("Multiple source shifts claim the same MitHF shift")]
    SharedDestination,
    #[error("Destination changed during synchronization; stopped before the next write")]
    DestinationChanged,
    #[error("A new unapproved write appeared; stopped")]
    UnapprovedWrite,
    #[error("Saved values did not match the plan; stopped for reconciliation")]
    VerificationFailed,
    #[error("Final reconciliation found unresolved changes")]
    FinalUnresolved,
    #[error("The requested step is not in the approved plan")]
    UnknownStep,
}

/// Match Python's approval hash, including ordered items, sorted object keys,
/// ASCII escapes, JSON spacing and microsecond datetime formatting.
/// Presentation text, system, reason and generation time are omitted as in Python.
pub fn plan_digest(plan: &SyncPlan) -> Result<String, ApprovalError> {
    json_digest(&json!({
        "from": isoformat(plan.starts_at),
        "to": isoformat(plan.ends_at),
        "items": plan.items.iter().map(|item| json!({
            "source": item.source_key, "step": item.step_key,
            "outcome": item.outcome, "destination": item.destination_id,
            "payload": item.payload,
        })).collect::<Vec<_>>(),
    }))
}

/// Hash the exact values persisted in a step's `source_hash` recovery field.
pub fn payload_digest(item: &PlanItem) -> Result<String, ApprovalError> {
    json_digest(&Value::Object(item.payload.clone()))
}

// Current transfer payloads use strings and integers, never floats. Reject
// floats instead of silently hashing a different representation from Python.
pub(crate) fn json_digest(value: &Value) -> Result<String, ApprovalError> {
    fn encode(value: &Value, output: &mut String) -> Result<(), ApprovalError> {
        match value {
            Value::Null => output.push_str("null"),
            Value::Bool(v) => output.push_str(if *v { "true" } else { "false" }),
            Value::Number(v) if v.is_i64() || v.is_u64() => output.push_str(&v.to_string()),
            Value::Number(_) => return Err(ApprovalError::UnsupportedNumber),
            Value::String(v) => {
                let escaped = serde_json::to_string(v).expect("strings always serialize");
                for c in escaped.chars() {
                    if c.is_ascii() && c != '\u{7f}' {
                        output.push(c);
                    } else {
                        for unit in c.encode_utf16(&mut [0; 2]) {
                            output.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                }
            }
            Value::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    encode(value, output)?;
                }
                output.push(']');
            }
            Value::Object(values) => {
                output.push('{');
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_by_key(|(key, _)| *key);
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    encode(&Value::String(key.clone()), output)?;
                    output.push_str(": ");
                    encode(value, output)?;
                }
                output.push('}');
            }
        }
        Ok(())
    }
    let mut encoded = String::new();
    encode(value, &mut encoded)?;
    Ok(format!("{:x}", Sha256::digest(encoded.as_bytes())))
}

/// Borrows the freshly reconciled plan so it cannot change during validation.
/// Obtain this only after acquiring the apply lock and reading destinations.
#[derive(Debug)]
pub struct ApprovedPlan<'a> {
    plan: &'a SyncPlan,
}

#[derive(Debug, PartialEq)]
pub enum StepAction<'a> {
    Skip,
    AlreadyMatched(&'a PlanItem),
    Write(&'a PlanItem),
}

impl<'a> ApprovedPlan<'a> {
    pub fn validate(plan: &'a SyncPlan, expected_digest: &str) -> Result<Self, ApprovalError> {
        if plan_digest(plan)? != expected_digest {
            return Err(ApprovalError::PlanChanged);
        }
        if plan.items.iter().any(|item| is_blocker(item.outcome)) {
            return Err(ApprovalError::UnresolvedItems);
        }
        let mut claims = HashMap::new();
        for item in &plan.items {
            if item.system != PlanSystem::Mithf
                || item.step_key.split('#').next() != Some("mithf.create_shift")
            {
                continue;
            }
            if let Some(id) = item.destination_id.as_deref().filter(|id| !id.is_empty()) {
                if claims
                    .insert(id, &item.source_key)
                    .is_some_and(|source| source != &item.source_key)
                {
                    return Err(ApprovalError::SharedDestination);
                }
            }
        }
        Ok(Self { plan })
    }

    pub fn plan(&self) -> &SyncPlan {
        self.plan
    }

    /// Reconcile immediately before each step, in approved order. An ID can
    /// appear/change after creation or assignment; payload values cannot change.
    pub fn check_step<'b>(
        &self,
        index: usize,
        current: &'b SyncPlan,
    ) -> Result<StepAction<'b>, ApprovalError> {
        let original = self.original(index)?;
        if original.outcome == Outcome::Excluded {
            return Ok(StepAction::Skip);
        }
        let item = find_step(current, original).ok_or(ApprovalError::DestinationChanged)?;
        if item.payload != original.payload || is_blocker(item.outcome) {
            return Err(ApprovalError::DestinationChanged);
        }
        if item.outcome == Outcome::AlreadyMatched {
            return Ok(StepAction::AlreadyMatched(item));
        }
        if !is_write(item.outcome) || !is_write(original.outcome) {
            return Err(ApprovalError::UnapprovedWrite);
        }
        Ok(StepAction::Write(item))
    }

    /// A submitted write remains uncertain until a new destination read yields
    /// an exactly matching step. The orchestrator must persist that marker.
    pub fn check_read_back<'b>(
        &self,
        index: usize,
        current: &'b SyncPlan,
    ) -> Result<&'b PlanItem, ApprovalError> {
        let item =
            find_step(current, self.original(index)?).ok_or(ApprovalError::VerificationFailed)?;
        if item.outcome != Outcome::AlreadyMatched {
            return Err(ApprovalError::VerificationFailed);
        }
        Ok(item)
    }

    /// Call after the last complete destination read, before recording source
    /// snapshots. This mirrors Python's final reconciliation outcome check.
    pub fn check_final(&self, current: &SyncPlan) -> Result<(), ApprovalError> {
        if current
            .items
            .iter()
            .any(|item| !matches!(item.outcome, Outcome::AlreadyMatched | Outcome::Excluded))
        {
            return Err(ApprovalError::FinalUnresolved);
        }
        Ok(())
    }

    fn original(&self, index: usize) -> Result<&PlanItem, ApprovalError> {
        self.plan.items.get(index).ok_or(ApprovalError::UnknownStep)
    }
}

fn find_step<'a>(plan: &'a SyncPlan, original: &PlanItem) -> Option<&'a PlanItem> {
    plan.items
        .iter()
        .find(|item| item.source_key == original.source_key && item.step_key == original.step_key)
}

fn is_write(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::WouldCreate | Outcome::WouldUpdate)
}

fn is_blocker(outcome: Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Conflicted | Outcome::Review | Outcome::Failed | Outcome::PendingIntegration
    )
}
