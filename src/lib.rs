//! ghostwave library exports for testing and integration.

pub mod ai;
pub mod app;
pub mod clipboard;
pub mod config;
pub mod fonts;
pub mod keymap;
pub mod layout;
pub mod palette;
pub mod preview;
pub mod pty;
pub mod search;
pub mod session;
pub mod term;
pub mod theme;
pub mod visualizer;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
