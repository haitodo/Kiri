//! Context menu and system tray menu (`menu.rs`).
//!
//! Per Section 2.1, 2.2, 5.2, and 8.1 of spec.md:
//! - Right-clicking anywhere on the launcher (including the grip via `WM_NCRBUTTONUP` / `WM_CONTEXTMENU`)
//!   or right-clicking the tray icon opens this management menu instead of the default OS system menu.
//! - Because Win32 `TrackPopupMenu` requires the owner window to be foreground to close on outside click,
//!   Section 2.1 explicitly permits temporary foreground acquisition for the right-click management menu.

use crate::config::Orientation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    OpenSettings,
    ToggleOrientation,
    ResetPosition,
    SaveCurrentLayout,
    RestoreSavedLayout,
    RestoreAllMinimized,
    ExitAndRestore,
}

const IDM_SETTINGS: usize = 2001;
const IDM_TOGGLE_ORIENTATION: usize = 2002;
const IDM_RESET_POSITION: usize = 2003;
const IDM_SAVE_LAYOUT: usize = 2004;
const IDM_RESTORE_LAYOUT: usize = 2005;
const IDM_RESTORE_ALL_MINIMIZED: usize = 2006;
const IDM_EXIT: usize = 2007;

#[cfg(windows)]
pub fn show_context_menu(
    owner_hwnd: windows::Win32::Foundation::HWND,
    screen_x: i32,
    screen_y: i32,
    current_orientation: Orientation,
) -> Option<MenuCommand> {
    use windows::core::w;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow,
        TrackPopupMenu, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
    };

    unsafe {
        let hmenu = CreatePopupMenu().ok()?;

        let _ = AppendMenuW(hmenu, MF_STRING, IDM_SETTINGS, w!("設定(&S)..."));
        let orient_label = match current_orientation {
            Orientation::Horizontal => w!("縦型へ切替(&O)"),
            Orientation::Vertical => w!("横型へ切替(&O)"),
        };
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_TOGGLE_ORIENTATION, orient_label);
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_RESET_POSITION, w!("位置をリセット(&P)"));

        let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            hmenu,
            MF_STRING,
            IDM_SAVE_LAYOUT,
            w!("現在シーンのレイアウトを保存(&L)"),
        );
        let _ = AppendMenuW(
            hmenu,
            MF_STRING,
            IDM_RESTORE_LAYOUT,
            w!("保存済みレイアウトを復元(&Y)"),
        );

        let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            hmenu,
            MF_STRING,
            IDM_RESTORE_ALL_MINIMIZED,
            w!("Kiriが最小化したウィンドウをすべて復元(&R)"),
        );

        let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_EXIT, w!("終了(&X)"));

        // Per Section 2.1: TrackPopupMenu requires SetForegroundWindow + post WM_NULL so clicking outside closes it
        let _ = SetForegroundWindow(owner_hwnd);
        let cmd_id = TrackPopupMenu(
            hmenu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            screen_x,
            screen_y,
            0,
            owner_hwnd,
            None,
        );
        let _ = PostMessageW(owner_hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(hmenu);

        match cmd_id.0 as usize {
            IDM_SETTINGS => Some(MenuCommand::OpenSettings),
            IDM_TOGGLE_ORIENTATION => Some(MenuCommand::ToggleOrientation),
            IDM_RESET_POSITION => Some(MenuCommand::ResetPosition),
            IDM_SAVE_LAYOUT => Some(MenuCommand::SaveCurrentLayout),
            IDM_RESTORE_LAYOUT => Some(MenuCommand::RestoreSavedLayout),
            IDM_RESTORE_ALL_MINIMIZED => Some(MenuCommand::RestoreAllMinimized),
            IDM_EXIT => Some(MenuCommand::ExitAndRestore),
            _ => None,
        }
    }
}
