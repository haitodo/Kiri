//! Platform abstraction layer (`WindowSystem` trait) and Win32 implementations.
//!
//! By isolating all OS queries and window mutations behind `WindowSystem`,
//! the scene switching engine, window matching, ambiguity detection, and recovery journal
//! remain 100% pure logic testable with `FakeWindowSystem`.

#[cfg(windows)]
pub mod cursor_input;
pub mod fake;
pub mod hooks;
pub mod monitors;
pub mod tray;
pub mod windows;

use crate::config::RectData;
use serde::{Deserialize, Serialize};

/// Window show/minimize commands as specified in Section 5.3 & 5.4 of spec.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShowCmd {
    /// `SW_SHOWMINNOACTIVE` (7): Minimize without activating another window.
    MinimizeNoActivate,
    /// `SW_MINIMIZE` (6): Minimize the foreground window so the OS activates the next window in Z-order
    /// (used after restoring target scene windows to prevent keyboard input stranding on an invisible window, FR-18).
    MinimizeActivateNext,
    /// `SW_SHOWNOACTIVATE` (4): Restore an iconic window without stealing focus.
    /// Must only be sent after confirming `is_iconic == true`.
    RestoreNoActivate,
    /// `SW_RESTORE` (9): Restore and activate (used only when focus_on_switch is enabled or explicit restore).
    RestoreActivate,
}

/// Runtime snapshot of a single top-level window (Section 4.1 `WindowInstance`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInstance {
    pub hwnd: u64,
    pub pid: u32,
    pub process_start_time: u64,
    pub exe_path: String,
    pub window_class: String,
    pub title: String,
    pub aumid: Option<String>,
    /// Stable monitor ID to which this window is attributed (using pre-minimized rect when iconic).
    pub monitor_id: String,
    pub is_visible: bool,
    pub is_iconic: bool,
    /// False when the window is disabled (e.g. a modal confirmation dialog is open, FR-17).
    pub is_enabled: bool,
    pub is_hung: bool,
    /// True if maximized or if iconic with `WPF_RESTORETOMAXIMIZED`.
    pub is_maximized: bool,
    pub has_taskbar_button: bool,
    pub is_elevated: bool,
    /// Workspace coordinates from `WINDOWPLACEMENT.rcNormalPosition`.
    pub normal_rect: RectData,
    /// Screen coordinates from `GetWindowRect`.
    pub window_rect: RectData,
    /// Z-order index from `EnumWindows` (0 = topmost).
    pub z_order: usize,
}

impl WindowInstance {
    /// Checks whether this live `WindowInstance` represents the exact same OS window instance
    /// as a previously recorded `(hwnd, pid, process_start_time)` tuple.
    pub fn matches_runtime_identity(&self, hwnd: u64, pid: u32, start_time: u64) -> bool {
        self.hwnd == hwnd && self.pid == pid && self.process_start_time == start_time
    }
}

/// Information about an attached display monitor (Section 2.2 & 4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfoData {
    pub monitor_id: String,
    pub friendly_name: String,
    pub monitor_rect: RectData,
    pub work_area: RectData,
    pub dpi: u32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    WindowNotFound(u64),
    AccessDenied(u64),
    HungWindow(u64),
    OsError(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::WindowNotFound(h) => write!(f, "Window 0x{:X} no longer exists", h),
            PlatformError::AccessDenied(h) => {
                write!(f, "Access denied (UIPI / elevated process) for window 0x{:X}", h)
            }
            PlatformError::HungWindow(h) => write!(f, "Window 0x{:X} is not responding", h),
            PlatformError::OsError(msg) => write!(f, "OS error: {}", msg),
        }
    }
}

/// Abstraction over Win32 window enumeration and state control (Section 7.1).
pub trait WindowSystem {
    /// Enumerate candidate top-level windows on the current virtual desktop in Z-order (0 = top).
    /// Excludes cloaked windows, other virtual desktop windows, owned windows, and Kiri's own windows.
    fn enumerate_windows(&self) -> Vec<WindowInstance>;

    /// Query the live state of a specific HWND (even if it became invisible after minimizing to system tray).
    fn query_window(&self, hwnd: u64) -> Option<WindowInstance>;

    /// Issue an asynchronous `ShowWindowAsync` command to another process's window.
    fn show_window_async(&mut self, hwnd: u64, cmd: ShowCmd) -> Result<(), PlatformError>;

    /// Bring a window to the top of the Z-order without necessarily activating it.
    fn bring_to_top(&mut self, hwnd: u64) -> bool;

    /// Get the current foreground window HWND (`GetForegroundWindow`).
    fn get_foreground_window(&self) -> Option<u64>;

    /// Request moving foreground focus to `hwnd` (`SetForegroundWindow`).
    fn set_foreground_window(&mut self, hwnd: u64) -> bool;

    /// Apply explicit saved placement (`SetWindowPlacement`) during manual layout restoration.
    fn set_window_placement(
        &mut self,
        hwnd: u64,
        normal_rect: RectData,
        maximized: bool,
    ) -> Result<(), PlatformError>;

    /// Enumerate currently connected display monitors with stable IDs, work areas, and DPI.
    fn monitors(&self) -> Vec<MonitorInfoData>;
}
