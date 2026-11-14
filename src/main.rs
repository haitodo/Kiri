#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Kiri entry point:
//! - Installs panic hook for crash diagnostics
//! - Ensures Per-Monitor DPI Awareness V2
//! - Enforces session-local single instance via `CreateMutexW("Local\\Kiri-...")`
//!   and notifies the existing instance (`AllowSetForegroundWindow` + `WM_APP_OPEN_SETTINGS`)
//! - Runs the event-driven Win32 `GetMessageW` loop with zero idle polling.

pub mod app;
pub mod config;
pub mod diagnostics;
pub mod launcher;
pub mod platform;
pub mod recovery;
pub mod scenes;
pub mod settings;

use crate::app::AppController;
use crate::config::ConfigManager;
use crate::diagnostics::{init_logger, log_error, log_info};
use crate::launcher::window::{LauncherWindowHost, WM_APP_OPEN_SETTINGS};
use crate::platform::windows::Win32WindowSystem;
use crate::recovery::RecoveryManager;

fn main() {
    init_logger(None, false);

    // Install panic hook (Section 7.1)
    std::panic::set_hook(Box::new(|info| {
        log_error("panic", &format!("Unhandled panic: {}", info));
    }));

    #[cfg(windows)]
    {
        enable_per_monitor_dpi_v2();

        let Some(_mutex_guard) = acquire_single_instance_or_notify_existing() else {
            return;
        };

        log_info("startup", "Kiri starting up");

        let config_mgr = ConfigManager::new(ConfigManager::default_config_path());
        let recovery = RecoveryManager::new(RecoveryManager::default_journal_path());
        let sys = Box::new(Win32WindowSystem::new());

        let app = AppController::new(config_mgr, recovery, sys);
        match LauncherWindowHost::create_and_show(app) {
            Ok(_host) => {
                run_message_loop();
            }
            Err(e) => {
                log_error("startup", &format!("Failed to initialize launcher window: {}", e));
            }
        }
    }
}

#[cfg(windows)]
fn enable_per_monitor_dpi_v2() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(windows)]
struct SingleInstanceMutex(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for SingleInstanceMutex {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Acquire `Local\Kiri-{GUID}` mutex. If another instance is already running in this user session,
/// grant it foreground permission (`AllowSetForegroundWindow`) and post `WM_APP_OPEN_SETTINGS` (Section 7.2 & 8.2).
#[cfg(windows)]
fn acquire_single_instance_or_notify_existing() -> Option<SingleInstanceMutex> {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    };

    unsafe {
        let h_mutex = CreateMutexW(
            None,
            false,
            w!("Local\\Kiri-8B3F6A20-5E19-4C94-A123-7D9C0E5F1101"),
        )
        .ok()?;

        if GetLastError() == ERROR_ALREADY_EXISTS {
            log_info(
                "startup",
                "Existing Kiri instance detected; forwarding focus and opening settings",
            );
            if let Ok(existing_hwnd) = FindWindowW(w!("KiriLauncherWindowClass"), None) {
                if !existing_hwnd.0.is_null() {
                    let mut existing_pid = 0u32;
                    GetWindowThreadProcessId(existing_hwnd, Some(&mut existing_pid));
                    if existing_pid != 0 {
                        let _ = AllowSetForegroundWindow(existing_pid);
                    }
                    let _ = PostMessageW(
                        existing_hwnd,
                        WM_APP_OPEN_SETTINGS,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            let _ = windows::Win32::Foundation::CloseHandle(h_mutex);
            return None;
        }

        Some(SingleInstanceMutex(h_mutex))
    }
}

/// Event-driven native Win32 message loop (`GetMessageW` blocks when idle, consuming 0% CPU).
#[cfg(windows)]
fn run_message_loop() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if msg.message == crate::settings::window::WM_SETTINGS_REQUEST {
                crate::settings::window::run_file_request();
            }
        }
    }
}
