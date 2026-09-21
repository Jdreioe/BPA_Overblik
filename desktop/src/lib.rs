//! Desktop presentation and the legacy worker connection during migration.
//! Native week views use the core models directly; the default desktop still
//! uses the Python worker for setup and its existing workflow.

pub mod preview;
pub mod protocol;
pub mod worker;
