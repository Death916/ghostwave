//! ghostwave library exports for testing and integration.

pub mod clipboard;
pub mod config;
pub mod keymap;
pub mod layout;
pub mod program_status;
pub mod pty;
pub mod search;
pub mod session;
pub mod term;
pub mod theme;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
