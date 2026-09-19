//! Shared library for the Iced desktop shell: the typed v1 worker protocol
//! and the worker supervisor. Kept as a library so integration tests can
//! exercise the real Python worker round-trip.

pub mod protocol;
pub mod worker;
