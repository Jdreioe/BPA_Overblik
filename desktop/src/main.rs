//! Danish Iced desktop shell for weekly shift transfers.
//!
//! The app runs entirely on the Rust core: setup, live reads, planning and
//! verified transfers. Network, browser and SQLite work runs on blocking
//! threads, never on the UI thread.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod native;
mod selfcheck;
mod setup;
mod widgets;

fn main() -> iced::Result {
    if std::env::args().any(|argument| argument == "--self-check") {
        return selfcheck::run();
    }
    native::run()
}
