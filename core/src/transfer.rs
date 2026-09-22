//! Asynchronous transfer orchestration. Network/browser calls are supplied by
//! adapters; all SQLite and filesystem work runs on Tokio's blocking pool.
use std::{
    error::Error,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, FixedOffset, Utc};

use crate::{
    build_plan, payload_digest, reconciliation_range, ApplyGuard, ApprovalError, ApprovedPlan,
    DestinationSnapshot, Outcome, PlanItem, PlanRequest, PlanSystem, PlanningConfig, PlanningError,
    SourceShift, StateError, StepAction, StepRecord, SyncPlan, SyncState,
};

/// Adapters must reject incomplete reads. A write may return a changed destination
/// ID, but a successful response never replaces the runner's fresh read-back.
/// Adapter errors must not contain credentials or private source content.
pub trait Destinations {
    type Error: Error + Send + Sync + 'static;

    fn read(
        &mut self,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> impl Future<Output = Result<DestinationSnapshot, Self::Error>> + Send;

    fn write(
        &mut self,
        item: &PlanItem,
        snapshot: &DestinationSnapshot,
    ) -> impl Future<Output = Result<Option<String>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct ApplyRequest {
    pub config: PlanningConfig,
    pub shifts: Vec<SourceShift>,
    pub range_start: DateTime<FixedOffset>,
    pub range_end: DateTime<FixedOffset>,
    /// Fixed throughout this run.
    pub now: DateTime<FixedOffset>,
    pub expected_digest: String,
}

/// Public progress data contains only stable identifiers needed by callers.
/// Payloads and source text stay inside the transfer runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferOperation {
    pub source_key: String,
    pub step_key: String,
    pub system: PlanSystem,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferEvent {
    Started(TransferOperation),
    Uncertain(TransferOperation),
    Verified(TransferOperation),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplyOutcome {
    Completed {
        plan: SyncPlan,
        verified_at: DateTime<FixedOffset>,
    },
    Stopped {
        plan: SyncPlan,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Planning(#[from] PlanningError),
    #[error(transparent)]
    Approval(#[from] ApprovalError),
    // Do not include adapter response contents in user-facing errors.
    #[error("Destination request failed; review synchronization state before retrying")]
    Destination(#[source] Box<dyn Error + Send + Sync>),
    #[error("Synchronization state worker failed")]
    StateWorker,
}

// Blocking jobs retain this owner too, so cancellation cannot release the apply
// lock while an already-started SQLite commit is still running.
struct LockedState {
    state: SyncState,
    _guard: ApplyGuard,
}

type SharedState = Arc<Mutex<LockedState>>;

/// Requires a Tokio runtime. Owns the account-state connection and apply lock.
/// Dropping this future releases the lock and leaves any committed uncertain
/// marker intact. Adapters must not spawn detached writes after cancellation.
/// Progress receives structured start and verification events without payloads.
/// The callback must return promptly; it runs on the async executor.
pub async fn apply_plan(
    request: ApplyRequest,
    state_path: PathBuf,
    destinations: &mut (impl Destinations + Send),
    progress: impl FnMut(TransferEvent) + Send,
) -> Result<SyncPlan, TransferError> {
    match apply_plan_controlled(request, state_path, destinations, progress, || false).await? {
        ApplyOutcome::Completed { plan, .. } => Ok(plan),
        ApplyOutcome::Stopped { .. } => unreachable!("an uncontrolled apply cannot stop"),
    }
}

/// Run an approved transfer and honor stop requests between operations.
/// A request that arrives during a write takes effect after its read-back.
pub async fn apply_plan_controlled(
    request: ApplyRequest,
    state_path: PathBuf,
    destinations: &mut (impl Destinations + Send),
    mut progress: impl FnMut(TransferEvent) + Send,
    should_stop: impl Fn() -> bool + Send + Sync,
) -> Result<ApplyOutcome, TransferError> {
    let state = tokio::task::spawn_blocking(move || -> Result<_, StateError> {
        let state = SyncState::open(state_path)?;
        let guard = state.exclusive_apply()?;
        Ok(LockedState {
            state,
            _guard: guard,
        })
    })
    .await
    .map_err(|_| TransferError::StateWorker)??;
    let state = Arc::new(Mutex::new(state));
    let request = Arc::new(request);
    let (read_start, read_end) =
        reconciliation_range(&request.shifts, request.range_start, request.range_end);
    let snapshot = destinations
        .read(read_start, read_end)
        .await
        .map_err(destination_error)?;
    let initial = plan(&state, &request, snapshot).await?;
    let approved = ApprovedPlan::validate(&initial, &request.expected_digest)?;
    if should_stop() {
        return Ok(ApplyOutcome::Stopped {
            plan: initial.clone(),
        });
    }

    for (index, original) in approved.plan().items.iter().enumerate() {
        if original.outcome == Outcome::Excluded {
            continue;
        }
        let snapshot = destinations
            .read(read_start, read_end)
            .await
            .map_err(destination_error)?;
        let current = plan(&state, &request, snapshot.clone()).await?;
        let item = match approved.check_step(index, &current)? {
            StepAction::Skip => continue,
            StepAction::AlreadyMatched(item) => {
                record(&state, item, "verified", item.destination_id.clone()).await?;
                if matches!(
                    original.outcome,
                    Outcome::WouldCreate | Outcome::WouldUpdate
                ) {
                    progress(TransferEvent::Verified(operation(item)));
                }
                continue;
            }
            StepAction::Write(item) => item,
        };
        if should_stop() {
            return Ok(ApplyOutcome::Stopped { plan: current });
        }
        let operation = operation(item);
        progress(TransferEvent::Started(operation.clone()));
        // Commit before submission, so timeout, cancellation or process death
        // cannot make an ambiguous request look safe to repeat.
        record(&state, item, "uncertain", item.destination_id.clone()).await?;
        progress(TransferEvent::Uncertain(operation.clone()));
        let id = destinations
            .write(item, &snapshot)
            .await
            .map_err(destination_error)?
            .filter(|id| !id.is_empty());
        if let Some(id) = &id {
            record(&state, item, "uncertain", Some(id.clone())).await?;
        }
        if let Some(id) = id {
            let (base, suffix) = item
                .step_key
                .split_once('#')
                .unwrap_or((&item.step_key, ""));
            if base == "mithf.assign_helper" {
                // Booking can replace a MitHF shift ID. Preserve the parent
                // payload/hash, but follow that segment's newly returned ID.
                let parent_key = if suffix.is_empty() {
                    "mithf.create_shift".into()
                } else {
                    format!("mithf.create_shift#{suffix}")
                };
                let source_key = item.source_key.clone();
                state_call(&state, move |state| {
                    if let Some(mut parent) = state.get_step(&source_key, &parent_key)? {
                        parent.status = "verified".into();
                        parent.destination_id = Some(id);
                        parent.error = None;
                        state.record_step(&parent, Utc::now().fixed_offset())?;
                    }
                    Ok(())
                })
                .await?;
            }
        }
        let snapshot = destinations
            .read(read_start, read_end)
            .await
            .map_err(destination_error)?;
        let verified_plan = plan(&state, &request, snapshot).await?;
        let verified = approved.check_read_back(index, &verified_plan)?;
        record(
            &state,
            verified,
            "verified",
            verified.destination_id.clone(),
        )
        .await?;
        progress(TransferEvent::Verified(operation));
    }
    let snapshot = destinations
        .read(read_start, read_end)
        .await
        .map_err(destination_error)?;
    let final_plan = plan(&state, &request, snapshot).await?;
    approved.check_final(&final_plan)?;
    let verified_at = Utc::now().fixed_offset();
    let source_keys: std::collections::BTreeSet<_> = final_plan
        .items
        .iter()
        .filter(|item| item.outcome == Outcome::AlreadyMatched)
        .map(|item| item.source_key.clone())
        .collect();
    state_call(&state, move |state| {
        for shift in &request.shifts {
            if source_keys.contains(&shift.key()) {
                state.record_source_snapshot(shift, verified_at)?;
            }
        }
        Ok(())
    })
    .await?;
    Ok(ApplyOutcome::Completed {
        plan: final_plan,
        verified_at,
    })
}

fn operation(item: &PlanItem) -> TransferOperation {
    TransferOperation {
        source_key: item.source_key.clone(),
        step_key: item.step_key.clone(),
        system: item.system,
    }
}

fn destination_error(error: impl Error + Send + Sync + 'static) -> TransferError {
    TransferError::Destination(Box::new(error))
}

async fn state_call<T: Send + 'static>(
    state: &SharedState,
    operation: impl FnOnce(&mut SyncState) -> Result<T, TransferError> + Send + 'static,
) -> Result<T, TransferError> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let mut state = state.lock().map_err(|_| TransferError::StateWorker)?;
        operation(&mut state.state)
    })
    .await
    .map_err(|_| TransferError::StateWorker)?
}

async fn plan(
    state: &SharedState,
    request: &Arc<ApplyRequest>,
    snapshot: DestinationSnapshot,
) -> Result<SyncPlan, TransferError> {
    let request = Arc::clone(request);
    state_call(state, move |state| {
        Ok(build_plan(
            &PlanRequest {
                config: &request.config,
                shifts: &request.shifts,
                destination: &snapshot,
                range_start: request.range_start,
                range_end: request.range_end,
                now: request.now,
                live: true,
            },
            state,
        )?)
    })
    .await
}

async fn record(
    state: &SharedState,
    item: &PlanItem,
    status: &'static str,
    id: Option<String>,
) -> Result<(), TransferError> {
    let record = StepRecord {
        source_key: item.source_key.clone(),
        step_key: item.step_key.clone(),
        status: status.into(),
        destination_id: id.filter(|id| !id.is_empty()),
        source_hash: payload_digest(item)?,
        synced_payload: item.payload.clone(),
        error: None,
    };
    state_call(state, move |state| {
        Ok(state.record_step(&record, Utc::now().fixed_offset())?)
    })
    .await
}
