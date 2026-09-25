//! Shared domain logic for the desktop app and command-line workflows.
//!
//! The core owns synchronization behavior. UI, network, and browser adapters
//! live outside this crate and exchange typed values with it.

mod absence;
mod approval;
pub mod fixture;
pub mod ical;
pub mod live;
mod models;
mod parser;
mod planner;
mod planning_models;
mod reconciliation;
pub mod sheets;
mod source_rules;
pub mod standard_time;
mod state;
mod transfer;
pub mod workbook;

pub use absence::{
    absence_markings, default_absences, parse_absences, AbsenceMarking, AbsenceParseResult,
    AbsencePart, AbsenceReason, DuosAbsence,
};
pub use models::{
    ParseIssue, ParseIssueCode, SourceComment, SourceMarker, SourceShift, SpsInterval,
    SpsParseResult, TimeInterval,
};
pub use parser::{parse_sps_instructions, NOTES_SOURCE_ID};
pub use source_rules::{
    classify_source_title, is_marker, marker_titles, MeetingCategory, SourceTitle,
};
pub use state::{ApplyGuard, StateError, StepRecord, SyncState};

pub use planner::{build_plan, reconciliation_range, PlanRequest, PlanningError};
pub use planning_models::{
    segment_step, DestinationSnapshot, DuosRegistration, HelperMapping, MitHfShift, Outcome,
    PlanItem, PlanSystem, PlanningConfig, SyncPlan,
};

pub use approval::{payload_digest, plan_digest, ApprovalError, ApprovedPlan, StepAction};

pub use transfer::{
    apply_plan, apply_plan_controlled, ApplyOutcome, ApplyRequest, Destinations, TransferError,
    TransferEvent, TransferOperation,
};
