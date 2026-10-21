//! ghostwave library exports for testing and integration.

pub mod clipboard;
pub mod config;
pub mod font_query;
pub mod keymap;
pub mod layout;
pub mod pty;
pub mod search;
pub mod session;
pub mod term;
pub mod theme;
pub mod ui_fonts;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
