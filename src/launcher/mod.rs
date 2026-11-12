//! Launcher UI and shortcut opening.

#[cfg(windows)]
mod cursor_popup;
pub mod input;
pub mod menu;
pub mod overflow;
pub mod paint;
#[cfg(windows)]
mod shortcuts;
pub mod window;
