//! Shared domain logic for the desktop app and command-line workflows.
//!
//! The core owns synchronization behavior. UI, network, and browser adapters
//! live outside this crate and exchange typed values with it.

mod models;
mod parser;
mod source_rules;

pub use models::{
    ParseIssue, ParseIssueCode, SourceComment, SourceShift, SpsInterval, SpsParseResult,
    TimeInterval,
};
pub use parser::{parse_sps_instructions, NOTES_SOURCE_ID};
pub use source_rules::{classify_source_title, MeetingCategory, SourceTitle};
