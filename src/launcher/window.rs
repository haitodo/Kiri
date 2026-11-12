//! Native Win32 Launcher Window (`window.rs`).
//!
//! Implements all Win32 message handling specified in Sections 2.1, 2.2, 5.1, 7.1, 7.2, 7.3, and 8.2:
//! - `WS_POPUP` with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
//! - `WM_MOUSEACTIVATE` -> `MA_NOACTIVATE` (clicks never steal foreground focus)
//! - `WM_NCHITTEST` -> `HTCAPTION` strictly on the grip area; `HTCLIENT` elsewhere
//! - `WM_NCLBUTTONDBLCLK` -> resets position without maximizing
//! - `WM_NCRBUTTONUP` / `WM_CONTEXTMENU` -> shows custom context menu even on the grip
//! - `WM_EXITSIZEMOVE` -> clamps/snaps to work area and persists position once at drag end
//! - `WM_DPICHANGED` / `WM_DISPLAYCHANGE` / `WM_POWERBROADCAST` / `WM_WTSSESSION_CHANGE` ->
//!   debounced via a single-shot timer to re-validate monitor layout and DPI
//! - `WM_QUERYENDSESSION` / `WM_ENDSESSION` -> flushes journal without restoring windows on OS logoff
//! - `std::panic::catch_unwind` at the `extern "system" fn` boundary to prevent FFI abort.

use crate::app::AppController;
use crate::diagnostics::{log_error, log_info, log_warn};
use crate::launcher::input::{HitTarget, LauncherLayout};
use crate::launcher::menu::{show_context_menu, MenuCommand};
use crate::launcher::overflow::{OverflowPopup, WM_APP_OVERFLOW_SELECT};
use crate::launcher::paint::{paint_launcher, GdiThemeCache, PaintState};
use crate::platform::hooks::{WinEventHookGuard, WM_APP_WINEVENT};
use crate::platform::tray::{TrayIcon, WM_APP_TRAYICON};
use crate::scenes::switch::SwitchWarning;
use windows::core::PWSTR;
use windows::Win32::UI::Controls::{
    TOOLTIPS_CLASSW, TTF_ABSOLUTE, TTF_TRACK, TTM_ADDTOOLW, TTM_TRACKACTIVATE, TTM_TRACKPOSITION,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

/// Window class name used for single-instance discovery (`FindWindowW`).
pub const LAUNCHER_WINDOW_CLASS: &str = "KiriLauncherWindowClass";

/// Message sent by a 2nd process instance to open Settings on the running instance (Section 7.2 & 8.2).
pub const WM_APP_OPEN_SETTINGS: u32 = 0x8000 + 4; // WM_APP + 4
pub const WM_APP_SETTINGS_SAVED: u32 = 0x8000 + 5; // WM_APP + 5
pub const WM_APP_RESTORE_ALL: u32 = 0x8000 + 6; // WM_APP + 6

const TIMER_VERIFY_SWITCH: usize = 101;
const TIMER_DISPLAY_DEBOUNCE: usize = 102;
const HOTKEY_BASE_ID: i32 = 4000;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[cfg(windows)]
pub struct LauncherWindowHost {
    pub hwnd: windows::Win32::Foundation::HWND,
    pub tooltip_hwnd: windows::Win32::Foundation::HWND,
    pub tooltip_visible: bool,
    pub tracking_mouse_leave: bool,
    pub app: AppController,
    pub layout: LauncherLayout,
    pub hovered: HitTarget,
    pub theme: GdiThemeCache,
    pub overflow_popup: Option<Box<OverflowPopup>>,
    pub tray: Option<TrayIcon>,
    pub win_event_hook: Option<WinEventHookGuard>,
    pub registered_hotkey_count: usize,
}

#[cfg(windows)]
impl LauncherWindowHost {
    pub fn create_and_show(mut app: AppController) -> Result<Box<Self>, String> {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::RemoteDesktop::{
            WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowLongPtrW, ShowWindow,
            CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, SW_SHOWNOACTIVATE,
            WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };

        static REGISTER_ONCE: Once = Once::new();
        let class_name = w!("KiriLauncherWindowClass");

        let (x, y, w, h) = app.resolve_initial_or_updated_launcher_rect();
        let dpi = app.launcher_dpi;
        let layout = app.compute_layout(dpi);

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
                    lpfnWndProc: Some(launcher_wndproc),
                    hInstance: hinstance.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            if app.config.launcher.always_on_top {
                ex_style |= WS_EX_TOPMOST;
            }

            let hwnd = CreateWindowExW(
                ex_style,
                class_name,
                w!("Kiri"),
                WS_POPUP,
                x,
                y,
                w,
                h,
                HWND::default(),
                None,
                hinstance,
                None,
            )
            .map_err(|e| format!("CreateWindowExW failed: {}", e))?;

            let overflow_popup = OverflowPopup::create(hwnd).ok();
            let tray = Some(TrayIcon::new(hwnd));
            let win_event_hook = WinEventHookGuard::install(hwnd);

            let tooltip_hwnd = CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                None,
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX,
                ),
                0,
                0,
                0,
                0,
                hwnd,
                None,
                hinstance,
                None,
            )
            .unwrap_or_default();

            if !tooltip_hwnd.0.is_null() {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
                let mut ti = TTTOOLINFOW {
                    cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                    uFlags: TTF_TRACK | TTF_ABSOLUTE,
                    hwnd,
                    uId: 1,
                    ..Default::default()
                };
                let _ = SendMessageW(
                    tooltip_hwnd,
                    TTM_ADDTOOLW,
                    WPARAM(0),
                    LPARAM(&mut ti as *mut _ as isize),
                );
            }

            let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);

            let mut host = Box::new(Self {
                hwnd,
                tooltip_hwnd,
                tooltip_visible: false,
                tracking_mouse_leave: false,
                app,
                layout,
                hovered: HitTarget::None,
                theme: GdiThemeCache::new(dpi),
                overflow_popup,
                tray,
                win_event_hook,
                registered_hotkey_count: 0,
            });

            let ptr = &mut *host as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            host.register_configured_hotkeys();
            host.check_startup_recovery_prompt();

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(host)
        }
    }

    fn check_startup_recovery_prompt(&mut self) {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_ICONQUESTION, MB_TOPMOST, MB_YESNO,
        };

        let crash_candidates = self.app.check_startup_crash_recovery();
        if crash_candidates.is_empty() {
            return;
        }

        unsafe {
            let ans = MessageBoxW(
                self.hwnd,
                w!("前回の異常終了時に Kiri が最小化したウィンドウが残っています。\nこれらを今すぐ復元しますか？"),
                w!("Kiri - ウィンドウの復旧確認"),
                MB_YESNO | MB_ICONQUESTION | MB_TOPMOST,
            );
            if ans == IDYES {
                self.app.restore_all_minimized_windows();
            }
        }
    }

    pub fn register_configured_hotkeys(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
        };

        unsafe {
            for i in 0..self.registered_hotkey_count {
                let _ = UnregisterHotKey(self.hwnd, HOTKEY_BASE_ID + i as i32);
            }
            self.registered_hotkey_count = 0;

            for (idx, hk) in self.app.config.hotkeys.iter().enumerate() {
                if !hk.has_safe_modifiers() {
                    log_warn(
                        "hotkey",
                        &format!(
                            "Hotkey for scene '{}' (vk=0x{:X}) has no modifier keys; may conflict with trading app shortcuts",
                            hk.scene_id, hk.vk
                        ),
                    );
                }
                let id = HOTKEY_BASE_ID + idx as i32;
                if RegisterHotKey(self.hwnd, id, HOT_KEY_MODIFIERS(hk.modifiers), hk.vk).is_err() {
                    log_warn(
                        "hotkey",
                        &format!(
                            "Failed to register hotkey for scene '{}' (modifiers=0x{:X}, vk=0x{:X})",
                            hk.scene_id, hk.modifiers, hk.vk
                        ),
                    );
                }
                self.registered_hotkey_count = idx + 1;
            }
        }
    }

    pub fn apply_window_bounds(&mut self, x: i32, y: i32, w: i32, h: i32) {
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
        };

        self.layout = self.app.compute_layout(self.app.launcher_dpi);
        let z_hwnd = if self.app.config.launcher.always_on_top {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        unsafe {
            let _ = SetWindowPos(self.hwnd, z_hwnd, x, y, w, h, SWP_NOACTIVATE);
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    pub fn hide_tooltip(&mut self) {
        if self.tooltip_visible && !self.tooltip_hwnd.0.is_null() {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                hwnd: self.hwnd,
                uId: 1,
                ..Default::default()
            };
            unsafe {
                let _ = SendMessageW(
                    self.tooltip_hwnd,
                    TTM_TRACKACTIVATE,
                    WPARAM(0),
                    LPARAM(&mut ti as *mut _ as isize),
                );
            }
            self.tooltip_visible = false;
        }
    }

    pub fn update_tooltip(&mut self, target: &HitTarget, screen_x: i32, screen_y: i32) {
        if self.tooltip_hwnd.0.is_null() {
            return;
        }

        let text_opt = match target {
            HitTarget::SceneButton { scene_id, .. } => {
                let (visible, overflow) = self.app.config.split_visible_and_overflow();
                let scene_name = visible
                    .iter()
                    .chain(overflow.iter())
                    .find(|s| &s.id == scene_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| scene_id.clone());
                Some(scene_name)
            }
            HitTarget::OverflowButton => {
                if self.app.is_active_scene_in_overflow() {
                    let active_name = self
                        .app
                        .state_mgr
                        .active_scene_id()
                        .and_then(|id| self.app.config.scenes.iter().find(|s| s.id == id))
                        .map(|s| s.name.as_str())
                        .unwrap_or("選択中");
                    Some(format!("⋯ ({})", active_name))
                } else {
                    Some("その他のシーン (⋯)".to_string())
                }
            }
            HitTarget::Grip => Some("Kiri (ドラッグ移動 / Wクリックでリセット)".to_string()),
            HitTarget::None => None,
        };

        if let Some(text) = text_opt {
            use windows::Win32::Foundation::{LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
            let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                hwnd: self.hwnd,
                uId: 1,
                lpszText: PWSTR(wide.as_mut_ptr()),
                ..Default::default()
            };
            unsafe {
                let _ = SendMessageW(
                    self.tooltip_hwnd,
                    TTM_UPDATETIPTEXTW,
                    WPARAM(0),
                    LPARAM(&mut ti as *mut _ as isize),
                );
                let pos = (((screen_y + 22) as u32) << 16) | (((screen_x + 12) as u32) & 0xFFFF);
                let _ = SendMessageW(
                    self.tooltip_hwnd,
                    TTM_TRACKPOSITION,
                    WPARAM(0),
                    LPARAM(pos as isize),
                );
                if !self.tooltip_visible {
                    let _ = SendMessageW(
                        self.tooltip_hwnd,
                        TTM_TRACKACTIVATE,
                        WPARAM(1),
                        LPARAM(&mut ti as *mut _ as isize),
                    );
                    self.tooltip_visible = true;
                }
            }
        } else {
            self.hide_tooltip();
        }
    }

    pub fn trigger_scene_switch(&mut self, scene_id: &str) {
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        use windows::Win32::UI::WindowsAndMessaging::{
            KillTimer, MessageBoxW, SetTimer, IDYES, MB_ICONINFORMATION, MB_TOPMOST, MB_YESNO,
        };

        let outcome = self.app.switch_to_scene(scene_id);

        if outcome
            .warnings
            .contains(&SwitchWarning::TargetMonitorNotConfigured)
        {
            // Per Section 0 & 6: Prompt user to select target monitor on first use, proposing launcher's monitor
            unsafe {
                let ans = MessageBoxW(
                    self.hwnd,
                    w!("制御対象モニターがまだ設定されていません。\n現在ランチャーがあるモニターを制御対象モニターとして設定しますか？"),
                    w!("Kiri - 初回モニター設定"),
                    MB_YESNO | MB_ICONINFORMATION | MB_TOPMOST,
                );
                if ans == IDYES && !self.app.launcher_monitor_id.is_empty() {
                    self.app.config.target.monitor_id = Some(self.app.launcher_monitor_id.clone());
                    let _ = self.app.config_mgr.save(&self.app.config);
                    // Retry the switch now that target monitor is configured
                    let retry = self.app.switch_to_scene(scene_id);
                    if retry.verification.is_some() {
                        let _ = KillTimer(self.hwnd, TIMER_VERIFY_SWITCH);
                        let _ = SetTimer(self.hwnd, TIMER_VERIFY_SWITCH, 500, None);
                    }
                }
                let _ = InvalidateRect(self.hwnd, None, false);
            }
            return;
        }

        if !outcome.was_noop_same_scene {
            unsafe {
                if outcome.verification.is_some() {
                    let _ = KillTimer(self.hwnd, TIMER_VERIFY_SWITCH);
                    let _ = SetTimer(self.hwnd, TIMER_VERIFY_SWITCH, 500, None);
                }
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
    }

    pub fn handle_menu_command(&mut self, cmd: MenuCommand) {
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        use windows::Win32::UI::WindowsAndMessaging::PostQuitMessage;

        if let Some(ref mut ov) = self.overflow_popup {
            ov.hide();
        }

        match cmd {
            MenuCommand::OpenSettings => {
                self.open_settings_dialog();
            }
            MenuCommand::ToggleOrientation => {
                let (x, y, w, h) = self.app.toggle_orientation();
                self.apply_window_bounds(x, y, w, h);
            }
            MenuCommand::ResetPosition => {
                let (x, y, w, h) = self.app.reset_launcher_position();
                self.apply_window_bounds(x, y, w, h);
            }
            MenuCommand::SaveCurrentLayout => {
                let _ = self.app.save_active_scene_layout();
            }
            MenuCommand::RestoreSavedLayout => {
                let _ = self.app.restore_active_scene_layout();
            }
            MenuCommand::RestoreAllMinimized => {
                let _ = self.app.restore_all_minimized_windows();
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
            MenuCommand::ExitAndRestore => {
                self.app.on_normal_shutdown();
                if let Some(ref mut tray) = self.tray {
                    tray.remove();
                }
                unsafe {
                    PostQuitMessage(0);
                }
            }
        }
    }

    pub fn open_settings_dialog(&mut self) {
        crate::settings::window::SettingsWindow::show(self.hwnd, &self.app);
    }
}

#[cfg(windows)]
unsafe extern "system" fn launcher_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use crate::config::RectData;
    use windows::Win32::Foundation::{LRESULT, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer, SetTimer,
        GWLP_USERDATA, HTCAPTION, HTCLIENT, MA_NOACTIVATE, WM_CONTEXTMENU, WM_DESTROY,
        WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_EXITSIZEMOVE, WM_HOTKEY, WM_LBUTTONUP,
        WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP, WM_NCHITTEST,
        WM_PAINT, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_TIMER,
        WM_WTSSESSION_CHANGE,
    };

    // Per Section 7.1: Catch any Rust panic at the FFI boundary to prevent immediate process abort
    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LauncherWindowHost;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let host = &mut *ptr;

        // Check dynamic TaskbarCreated message (Explorer restart, Section 7.2 & AT-28)
        if let Some(ref mut tray) = host.tray {
            let tb_msg = tray.taskbar_created_message_id();
            if tb_msg != 0 && msg == tb_msg {
                log_info("tray", "TaskbarCreated received; re-registering tray icon");
                tray.add_or_readd();
                return LRESULT(0);
            }
        }

        match msg {
            // Section 2.1: Never activate the launcher window on mouse click
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

            // Section 2.2: Only the dedicated grip area returns HTCAPTION for native OS window dragging
            WM_NCHITTEST => {
                let screen_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let screen_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut pt = POINT {
                    x: screen_x,
                    y: screen_y,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                match host.layout.hit_test(pt.x, pt.y) {
                    HitTarget::Grip => LRESULT(HTCAPTION as isize),
                    _ => LRESULT(HTCLIENT as isize),
                }
            }

            // Section 2.2: Double-clicking the grip resets launcher position (do not pass to DefWindowProcW)
            WM_NCLBUTTONDBLCLK => {
                if wparam.0 == HTCAPTION as usize {
                    let (x, y, w, h) = host.app.reset_launcher_position();
                    host.apply_window_bounds(x, y, w, h);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // Section 2.1: Right-clicking the grip (non-client HTCAPTION) opens our context menu, not OS system menu
            WM_NCRBUTTONUP => {
                host.hide_tooltip();
                let screen_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let screen_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if let Some(cmd) = show_context_menu(
                    hwnd,
                    screen_x,
                    screen_y,
                    host.app.config.launcher.orientation,
                ) {
                    host.handle_menu_command(cmd);
                }
                LRESULT(0)
            }

            WM_CONTEXTMENU | WM_RBUTTONUP => {
                host.hide_tooltip();
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                if let Some(cmd) =
                    show_context_menu(hwnd, pt.x, pt.y, host.app.config.launcher.orientation)
                {
                    host.handle_menu_command(cmd);
                }
                LRESULT(0)
            }

            // Section 2.2: Persist position only once when dragging ends
            WM_EXITSIZEMOVE => {
                host.hide_tooltip();
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    let (clamped_x, clamped_y) = host.app.on_launcher_drag_end(rc.left, rc.top);
                    if clamped_x != rc.left || clamped_y != rc.top {
                        let w = host.layout.width_px;
                        let h = host.layout.height_px;
                        host.apply_window_bounds(clamped_x, clamped_y, w, h);
                    }
                }
                LRESULT(0)
            }

            WM_MOUSEMOVE => {
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                if !host.tracking_mouse_leave {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    host.tracking_mouse_leave = true;
                }

                let new_target = host.layout.hit_test(client_x, client_y);
                if new_target != host.hovered {
                    host.hovered = new_target.clone();
                    let mut pt = POINT {
                        x: client_x,
                        y: client_y,
                    };
                    use windows::Win32::Graphics::Gdi::ClientToScreen;
                    let _ = ClientToScreen(hwnd, &mut pt);
                    host.update_tooltip(&new_target, pt.x, pt.y);
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_MOUSELEAVE => {
                host.tracking_mouse_leave = false;
                host.hovered = HitTarget::None;
                host.hide_tooltip();
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                host.hide_tooltip();
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                match host.layout.hit_test(client_x, client_y) {
                    HitTarget::SceneButton { scene_id, .. } => {
                        if let Some(ref mut ov) = host.overflow_popup {
                            ov.hide();
                        }
                        host.trigger_scene_switch(&scene_id);
                    }
                    HitTarget::OverflowButton => {
                        let (_, overflow_scenes) = host.app.config.split_visible_and_overflow();
                        if let Some(ref ov_btn_rc) = host.layout.overflow_button_rect {
                            let mut win_rc = RECT::default();
                            let _ = GetWindowRect(hwnd, &mut win_rc);
                            let anchor_screen = RectData {
                                left: win_rc.left + ov_btn_rc.left,
                                top: win_rc.top + ov_btn_rc.top,
                                right: win_rc.left + ov_btn_rc.right,
                                bottom: win_rc.top + ov_btn_rc.bottom,
                            };
                            let monitors = host.app.sys.monitors();
                            let work_area = monitors
                                .iter()
                                .find(|m| m.monitor_id == host.app.launcher_monitor_id)
                                .or_else(|| monitors.first())
                                .map(|m| m.work_area)
                                .unwrap_or(RectData {
                                    left: 0,
                                    top: 0,
                                    right: 1920,
                                    bottom: 1040,
                                });
                            let active_id =
                                host.app.state_mgr.active_scene_id().map(|s| s.to_string());
                            let dpi = host.app.launcher_dpi;
                            if let Some(ref mut ov) = host.overflow_popup {
                                ov.toggle_or_show(
                                    overflow_scenes,
                                    active_id,
                                    anchor_screen,
                                    work_area,
                                    dpi,
                                );
                            }
                        }
                    }
                    HitTarget::Grip | HitTarget::None => {}
                }
                LRESULT(0)
            }

            WM_APP_OVERFLOW_SELECT => {
                let idx = wparam.0;
                let (_, overflow_scenes) = host.app.config.split_visible_and_overflow();
                if let Some(scene) = overflow_scenes.get(idx) {
                    let scene_id = scene.id.clone();
                    host.trigger_scene_switch(&scene_id);
                }
                LRESULT(0)
            }

            WM_APP_WINEVENT => {
                host.app.process_queued_win_events();
                LRESULT(0)
            }

            WM_APP_TRAYICON => {
                let mouse_msg = (lparam.0 & 0xFFFF) as u32;
                if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_CONTEXTMENU {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    if let Some(cmd) =
                        show_context_menu(hwnd, pt.x, pt.y, host.app.config.launcher.orientation)
                    {
                        host.handle_menu_command(cmd);
                    }
                }
                LRESULT(0)
            }

            WM_APP_OPEN_SETTINGS => {
                host.open_settings_dialog();
                LRESULT(0)
            }

            WM_APP_SETTINGS_SAVED => {
                let ptr = wparam.0 as *mut crate::config::AppConfig;
                if !ptr.is_null() {
                    let new_cfg = *Box::from_raw(ptr);
                    host.app.config = new_cfg;
                    let _ = host.app.config_mgr.save(&host.app.config);
                    let (x, y, w, h) = host.app.resolve_initial_or_updated_launcher_rect();
                    host.apply_window_bounds(x, y, w, h);
                    host.register_configured_hotkeys();
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_APP_RESTORE_ALL => {
                host.app.restore_all_minimized_windows();
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_HOTKEY => {
                let hk_id = wparam.0 as i32 - HOTKEY_BASE_ID;
                if hk_id >= 0 {
                    if let Some(hk) = host.app.config.hotkeys.get(hk_id as usize) {
                        let scene_id = hk.scene_id.clone();
                        host.trigger_scene_switch(&scene_id);
                    }
                }
                LRESULT(0)
            }

            // Section 7.2: Aggregate rapid DPI / display / power / session change notifications via one-shot timer
            WM_DPICHANGED | WM_DISPLAYCHANGE | WM_POWERBROADCAST | WM_WTSSESSION_CHANGE => {
                let _ = KillTimer(hwnd, TIMER_DISPLAY_DEBOUNCE);
                let _ = SetTimer(hwnd, TIMER_DISPLAY_DEBOUNCE, 200, None);
                LRESULT(0)
            }

            WM_TIMER => {
                match wparam.0 {
                    TIMER_VERIFY_SWITCH => {
                        let _ = KillTimer(hwnd, TIMER_VERIFY_SWITCH);
                        let new_warnings = host.app.run_post_switch_verification();
                        if !new_warnings.is_empty() {
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                    TIMER_DISPLAY_DEBOUNCE => {
                        let _ = KillTimer(hwnd, TIMER_DISPLAY_DEBOUNCE);
                        let (x, y, w, h) = host.app.resolve_initial_or_updated_launcher_rect();
                        host.apply_window_bounds(x, y, w, h);
                    }
                    _ => {}
                }
                LRESULT(0)
            }

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let active_in_overflow = host.app.is_active_scene_in_overflow();
                let paint_state = PaintState {
                    layout: &host.layout,
                    scene_state: host.app.state_mgr.state(),
                    last_known_scene_id: host.app.state_mgr.last_known_scene_id(),
                    active_in_overflow,
                    hovered: &host.hovered,
                    has_warnings: !host.app.last_warnings.is_empty(),
                };
                paint_launcher(hdc, &mut host.theme, &paint_state);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // Section 7.2 & 8.2: On OS logoff/shutdown, do NOT restore windows; flush journal and exit cleanly
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    host.app.on_os_session_end();
                }
                LRESULT(0)
            }

            WM_DESTROY => LRESULT(0),

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    match result {
        Ok(lres) => lres,
        Err(_) => {
            log_error("wndproc", "Caught panic across FFI boundary in launcher_wndproc");
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
