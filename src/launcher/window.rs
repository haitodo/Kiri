//! Native Win32 Launcher Window (`window.rs`).
//!
//! Implements all Win32 message handling specified in Sections 2.1, 2.2, 5.1, 7.1, 7.2, 7.3, and 8.2:
//! - `WS_POPUP` with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
//! - `WM_MOUSEACTIVATE` -> `MA_NOACTIVATE` (clicks never steal foreground focus)
//! - `WM_NCHITTEST` -> `HTCLIENT`, with grip dragging handled by the launcher
//! - `WM_NCLBUTTONDBLCLK` -> resets position without maximizing
//! - `WM_NCRBUTTONUP` / `WM_CONTEXTMENU` -> shows custom context menu even on the grip
//! - `WM_EXITSIZEMOVE` -> clamps/snaps to work area and persists position once at drag end
//! - `WM_DPICHANGED` / `WM_DISPLAYCHANGE` / `WM_POWERBROADCAST` / `WM_WTSSESSION_CHANGE` ->
//!   debounced via a single-shot timer to re-validate monitor layout and DPI
//! - `WM_QUERYENDSESSION` / `WM_ENDSESSION` -> flushes journal without restoring windows on OS logoff
//! - `std::panic::catch_unwind` at the `extern "system" fn` boundary to prevent FFI abort.

use crate::app::AppController;
use crate::diagnostics::{log_error, log_info, log_warn};
use crate::launcher::cursor_popup::{
    CursorKeyOutcome, CursorPopup, PopupAction, WM_APP_CURSOR_POPUP_ACTION,
};
use crate::launcher::input::{
    wheel_scene_index, HitTarget, LauncherLayout, SceneWheel, CORNER_RADIUS_DIP,
};
use crate::launcher::menu::{show_context_menu, MenuCommand};
use crate::launcher::overflow::{OverflowPopup, WM_APP_OVERFLOW_SELECT};
use crate::launcher::paint::{paint_launcher, GdiThemeCache, PaintState};
use crate::platform::cursor_input::{
    CursorInput, WM_APP_CURSOR_POPUP_KEY, WM_APP_CURSOR_POPUP_WHEEL, WM_APP_DISMISS_CURSOR_POPUP,
    WM_APP_TOGGLE_CURSOR_POPUP,
};
use crate::platform::hooks::{WinEventHookGuard, WM_APP_WINEVENT};
use crate::platform::monitors::{dip_to_px, find_monitor_for_point};
use crate::platform::tray::{TrayIcon, WM_APP_TRAYICON};
use crate::scenes::manager::SceneState;
use crate::scenes::switch::{format_switch_warnings, SwitchSource, SwitchWarning};
use crate::settings::{window::{SettingsWindow, WM_SETTINGS_REQUEST}, SettingsPage};
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

const TIMER_VERIFY_SWITCH: usize = 101;
const TIMER_DISPLAY_DEBOUNCE: usize = 102;
const HOTKEY_BASE_ID: i32 = 4000;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Debug, Clone, Copy)]
pub struct LauncherDragState {
    pub grab_rel_x: f64,
    pub grab_rel_y: f64,
    pub start_cursor_x: i32,
    pub start_cursor_y: i32,
    pub moved: bool,
}

#[cfg(windows)]
pub struct LauncherWindowHost {
    pub hwnd: windows::Win32::Foundation::HWND,
    pub tooltip_hwnd: windows::Win32::Foundation::HWND,
    pub tooltip_visible: bool,
    pub tracking_mouse_leave: bool,
    pub drag_state: Option<LauncherDragState>,
    pub applying_bounds: bool,
    pub app: AppController,
    pub layout: LauncherLayout,
    pub hovered: HitTarget,
    pub pressed: HitTarget,
    pub theme: GdiThemeCache,
    pub overflow_popup: Option<Box<OverflowPopup>>,
    cursor_input: CursorInput,
    cursor_popup: Option<Box<CursorPopup>>,
    bar_wheel: SceneWheel,
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
            CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowLongPtrW,
            CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW,
            WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };

        static REGISTER_ONCE: Once = Once::new();
        let class_name = w!("KiriLauncherWindowClass");

        let (x, y, w, h) = app.resolve_initial_or_updated_launcher_rect();
        let dpi = app.launcher_dpi;
        log_info(
            "startup",
            &format!("Launcher bounds resolved: rect=({x}, {y}, {w}, {h}), dpi={dpi}"),
        );
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

            let theme = GdiThemeCache::new(dpi);
            let overflow_popup = OverflowPopup::create(hwnd, theme.gdiplus.clone()).ok();
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
                drag_state: None,
                applying_bounds: false,
                app,
                layout,
                hovered: HitTarget::None,
                pressed: HitTarget::None,
                theme,
                overflow_popup,
                cursor_input: CursorInput::new(hwnd),
                cursor_popup: None,
                bar_wheel: SceneWheel::default(),
                tray,
                win_event_hook,
                registered_hotkey_count: 0,
            });

            let ptr = &mut *host as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            host.register_configured_hotkeys();
            host.check_startup_recovery_prompt();
            host.apply_window_bounds(x, y, w, h);
            host.apply_launcher_settings();
            if host.app.config_mgr.is_read_only() {
                host.notify_tray(
                    "Kiri - 読み取り専用モード",
                    "新しいバージョンの config.json が読み込まれたため、設定の自動保存は無効です。",
                    true,
                );
            }
            log_info("startup", "Launcher window created and ready");
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

    pub fn notify_tray(&self, title: &str, message: &str, is_warning: bool) {
        if let Some(ref tray) = self.tray {
            tray.show_notification(title, message, is_warning);
        }
    }

    pub fn register_configured_hotkeys(&mut self) -> Vec<String> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
        };

        let mut failed_scenes: Vec<String> = Vec::new();
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
                    let scene_name = self
                        .app
                        .config
                        .scenes
                        .iter()
                        .find(|s| s.id == hk.scene_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| hk.scene_id.clone());
                    failed_scenes.push(scene_name);
                }
                self.registered_hotkey_count = idx + 1;
            }
        }
        if !failed_scenes.is_empty() {
            self.notify_tray(
                "Kiri - ホットキー登録の競合",
                &format!(
                    "他アプリとの競合により登録できないホットキーがありました: {}",
                    failed_scenes.join(", ")
                ),
                true,
            );
        }
        failed_scenes
    }

    fn apply_launcher_settings(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE};

        if let Err(error) = self.cursor_input.set_ctrl_enabled(self.app.config.launcher.ctrl_double_tap) {
            log_warn("hotkey", &format!("Could not enable Ctrl double-tap: {error}"));
        }
        let bar_hovered = self.app.config.launcher.show_bar
            && self.tracking_mouse_leave
            && !self.app.config.scenes.is_empty();
        let _ = self.cursor_input.set_bar_hovered(bar_hovered);
        let command = if self.app.config.launcher.show_bar {
            SW_SHOWNOACTIVATE
        } else {
            self.tracking_mouse_leave = false;
            self.bar_wheel = SceneWheel::default();
            self.hide_tooltip();
            if let Some(overflow) = &mut self.overflow_popup {
                overflow.hide();
            }
            SW_HIDE
        };
        unsafe { let _ = ShowWindow(self.hwnd, command); }
    }

    fn toggle_cursor_popup(&mut self) {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

        if self.cursor_popup.as_ref().is_some_and(|popup| popup.visible) {
            self.hide_cursor_popup();
            return;
        }
        let mut cursor = POINT::default();
        if self.drag_state.is_some() || unsafe { GetCursorPos(&mut cursor) }.is_err() {
            return;
        }
        let monitors = self.app.sys.monitors();
        let Some(monitor) = find_monitor_for_point(cursor.x, cursor.y, &monitors) else {
            return;
        };
        self.hide_tooltip();
        if let Some(overflow) = &mut self.overflow_popup {
            overflow.hide();
        }
        if self.cursor_popup.is_none() {
            match CursorPopup::create(self.hwnd, self.theme.gdiplus.clone()) {
                Ok(popup) => self.cursor_popup = Some(popup),
                Err(error) => {
                    log_error("launcher", &format!("Could not create cursor popup: {error}"));
                    return;
                }
            }
        }
        if let Some(popup) = &mut self.cursor_popup {
            match popup.show(cursor, monitor, &self.app.config, self.app.state_mgr.active_scene_id()) {
                Ok(input) => {
                    if let Err(error) = self.cursor_input.set_popup(Some(input)) {
                        log_warn("launcher", &format!("Could not hook popup mouse input: {error}"));
                    }
                }
                Err(error) => {
                    self.hide_cursor_popup();
                    log_error("launcher", &format!("Could not show cursor popup: {error}"));
                }
            }
        }
    }

    fn hide_cursor_popup(&mut self) {
        if let Some(popup) = &mut self.cursor_popup {
            let _ = self.cursor_input.set_popup(None);
            popup.hide();
        }
    }

    pub fn apply_window_bounds(&mut self, x: i32, y: i32, w: i32, h: i32) {
        use windows::Win32::Graphics::Gdi::{
            CreateRoundRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, HGDIOBJ,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
        };

        self.theme.ensure_dpi(self.app.launcher_dpi);
        self.layout = self.app.compute_layout(self.app.launcher_dpi);
        let z_hwnd = if self.app.config.launcher.always_on_top {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        let diameter = dip_to_px(CORNER_RADIUS_DIP * 2, self.app.launcher_dpi);
        self.applying_bounds = true;
        unsafe {
            let _ = SetWindowPos(self.hwnd, z_hwnd, x, y, w, h, SWP_NOACTIVATE);
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, diameter, diameter);
            if !rgn.is_invalid() && SetWindowRgn(self.hwnd, rgn, true) == 0 {
                let _ = DeleteObject(HGDIOBJ(rgn.0));
            }
            let _ = InvalidateRect(self.hwnd, None, false);
        }
        self.applying_bounds = false;
    }

    pub fn update_drag_position(
        &mut self,
        cursor_x: i32,
        cursor_y: i32,
        grab_rel_x: f64,
        grab_rel_y: f64,
    ) {
        use crate::platform::monitors::{clamp_and_snap_in_work_area, find_monitor_for_point};

        let monitors = self.app.sys.monitors();
        let Some(mon) = find_monitor_for_point(cursor_x, cursor_y, &monitors) else {
            return;
        };

        let target_dpi = mon.dpi.max(96);
        self.app.launcher_monitor_id = mon.monitor_id.clone();
        self.app.launcher_dpi = target_dpi;
        self.theme.ensure_dpi(target_dpi);
        self.layout = self.app.compute_layout(target_dpi);

        let w = self.layout.width_px;
        let h = self.layout.height_px;
        let raw_x = cursor_x - (grab_rel_x * w as f64).round() as i32;
        let raw_y = cursor_y - (grab_rel_y * h as f64).round() as i32;
        let (x, y) = clamp_and_snap_in_work_area(
            raw_x,
            raw_y,
            w,
            h,
            mon.work_area,
            target_dpi,
            self.app.config.launcher.snap_to_edges,
        );
        self.apply_window_bounds(x, y, w, h);
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
            HitTarget::Grip => {
                if self.app.last_warnings.is_empty() {
                    Some("Kiri (ドラッグ移動 / Wクリックでリセット)".to_string())
                } else {
                    Some(format!(
                        "Kiri — ⚠️ 警告 {}件（右クリックメニューから確認）",
                        self.app.last_warnings.len()
                    ))
                }
            }
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
                let off_x = crate::platform::monitors::dip_to_px(12, self.app.launcher_dpi);
                let off_y = crate::platform::monitors::dip_to_px(22, self.app.launcher_dpi);
                let tx = (screen_x + off_x) as i16 as u16 as u32;
                let ty = (screen_y + off_y) as i16 as u16 as u32;
                let pos = (ty << 16) | tx;
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

    fn trigger_scene_switch(&mut self, scene_id: &str, source: SwitchSource) {
        self.hide_cursor_popup();
        self.switch_scene(scene_id, source);
    }

    fn cycle_scene(&mut self, steps: i32, source: SwitchSource) {
        if steps == 0 {
            return;
        }
        let scenes = self.app.config.ordered_scenes();
        let active_id = self.app.state_mgr.active_scene_id();
        let active_index = scenes
            .iter()
            .position(|scene| Some(scene.id.as_str()) == active_id);
        let Some(index) = wheel_scene_index(
            scenes.len(),
            active_index,
            steps,
            self.app.config.launcher.wheel_wrap,
        ) else {
            return;
        };
        let scene_id = scenes[index].id.clone();
        self.hide_tooltip();
        if let Some(overflow) = &mut self.overflow_popup {
            overflow.hide();
        }
        self.switch_scene(&scene_id, source);
        if let Some(popup) = &mut self.cursor_popup {
            popup.update_active_scene(self.app.state_mgr.active_scene_id());
        }
    }

    fn switch_scene(&mut self, scene_id: &str, source: SwitchSource) {
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_ICONINFORMATION, MB_TOPMOST, MB_YESNO,
        };

        let outcome = self.app.switch_to_scene(scene_id, source);

        if outcome
            .warnings
            .contains(&SwitchWarning::TargetMonitorNotConfigured)
        {
            self.hide_cursor_popup();
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
                    let retry = self.app.switch_to_scene(scene_id, source);
                    if retry.verification.is_some() {
                        self.schedule_switch_verification();
                    }
                }
                let _ = InvalidateRect(self.hwnd, None, false);
            }
            return;
        }

        if !outcome.was_noop_same_scene {
            unsafe {
                if outcome.verification.is_some() {
                    self.schedule_switch_verification();
                }
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
    }

    fn schedule_switch_verification(&self) {
        use windows::Win32::Foundation::GetLastError;
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_VERIFY_SWITCH);
            if SetTimer(self.hwnd, TIMER_VERIFY_SWITCH, 500, None) == 0 {
                let error = GetLastError().0;
                if let Some(pending) = &self.app.pending_verification {
                    pending.record_timer_failure(error);
                }
            }
        }
    }

    pub fn handle_menu_command(&mut self, cmd: MenuCommand) {
        use windows::core::{w, HSTRING};
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, PostQuitMessage, IDYES, MB_ICONWARNING, MB_TOPMOST, MB_YESNO,
        };

        if let Some(ref mut ov) = self.overflow_popup {
            ov.hide();
        }

        match cmd {
            MenuCommand::ShowWarnings => {
                if self.app.last_warnings.is_empty() {
                    self.notify_tray("Kiri - 警告", "現在記録されている警告はありません。", false);
                    return;
                }
                let lines = format_switch_warnings(
                    &self.app.last_warnings,
                    &self.app.config.bindings,
                    &self.app.config.scenes,
                );
                let numbered: Vec<String> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{}. {}", i + 1, l))
                    .collect();
                let body = format!(
                    "直近のシーン切替で以下の警告が発生しました:\n\n{}\n\n確認後、ランチャーの警告インジケーター（オレンジ点）をクリアしますか？",
                    numbered.join("\n")
                );
                let body_w = HSTRING::from(body);
                let ans = unsafe {
                    MessageBoxW(
                        self.hwnd,
                        &body_w,
                        w!("Kiri - 最新の切替警告"),
                        MB_YESNO | MB_ICONWARNING | MB_TOPMOST,
                    )
                };
                if ans == IDYES {
                    self.app.last_warnings.clear();
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, false);
                    }
                }
            }
            MenuCommand::OpenCursorPopup => self.toggle_cursor_popup(),
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
                if self.app.save_active_scene_layout() {
                    let window_count = self
                        .app
                        .state_mgr
                        .active_scene_id()
                        .and_then(|id| {
                            self.app
                                .config
                                .layouts
                                .iter()
                                .rfind(|l| l.scene_id == id)
                                .map(|l| l.entries.len())
                        })
                        .unwrap_or(0);
                    self.notify_tray(
                        "Kiri - レイアウト保存",
                        &format!(
                            "現在シーンのレイアウトを保存しました（{} ウィンドウ）。",
                            window_count
                        ),
                        false,
                    );
                } else {
                    self.notify_tray(
                        "Kiri - レイアウト保存",
                        "アクティブなシーンがないか保存可能なウィンドウが見つかりませんでした。",
                        true,
                    );
                }
            }
            MenuCommand::RestoreSavedLayout => {
                if !matches!(self.app.state_mgr.state(), SceneState::Active(_)) {
                    self.notify_tray(
                        "Kiri - レイアウト復元",
                        "アクティブなシーンが未確定のため復元できません。先にシーンを選択してください。",
                        true,
                    );
                    return;
                }
                let restored = self.app.restore_active_scene_layout();
                if restored > 0 {
                    self.notify_tray(
                        "Kiri - レイアウト復元",
                        &format!("保存済みレイアウトを復元しました（{} ウィンドウ）。", restored),
                        false,
                    );
                } else {
                    self.notify_tray(
                        "Kiri - レイアウト復元",
                        "このシーン・モニタに一致する保存済みレイアウトまたは起動中ウィンドウがありません。",
                        true,
                    );
                }
            }
            MenuCommand::LaunchMissingSceneApps => {
                let (launched, skipped, errors) = self.app.launch_missing_apps_for_active_scene();
                if !errors.is_empty() {
                    self.notify_tray(
                        "Kiri - アプリ起動",
                        &format!(
                            "起動: {}件 / スキップ: {}件\n{}",
                            launched,
                            skipped,
                            errors.join("\n")
                        ),
                        true,
                    );
                } else if launched > 0 {
                    self.notify_tray(
                        "Kiri - アプリ起動",
                        &format!("現在シーンの未起動アプリを {} 件起動しました。", launched),
                        false,
                    );
                } else {
                    self.notify_tray(
                        "Kiri - アプリ起動",
                        "起動が必要な未起動アプリはありませんでした（UWP/未設定パスは対象外）。",
                        false,
                    );
                }
            }
            MenuCommand::RestoreAllMinimized => {
                let count = self.app.restore_all_minimized_windows();
                self.notify_tray(
                    "Kiri - 一括復元",
                    &format!("Kiri が最小化したウィンドウを {} 件復元しました。", count),
                    false,
                );
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

    fn execute_cursor_popup_action(&mut self, action: PopupAction) {
        self.hide_cursor_popup();
        match action {
            PopupAction::SwitchScene(scene_id) => {
                self.trigger_scene_switch(&scene_id, SwitchSource::PopupClick)
            }
            PopupAction::OpenShortcut(shortcut) => super::shortcuts::open(shortcut),
            PopupAction::OpenSettings => self.show_settings(SettingsPage::Shortcuts),
        }
    }

    pub fn open_settings_dialog(&mut self) {
        self.hide_cursor_popup();
        self.show_settings(SettingsPage::Scenes);
    }

    fn show_settings(&self, page: SettingsPage) {
        if let Err(error) = SettingsWindow::show(self.hwnd, page) {
            log_warn("settings", &error);
            self.notify_tray("Kiri 設定", "設定画面を開けませんでした。", true);
        }
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
    use windows::Win32::Foundation::{HWND, LRESULT, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
        SetCursor, SetTimer, GWLP_USERDATA, HTCAPTION, HTCLIENT, IDC_SIZEALL, MA_NOACTIVATE,
        WM_CAPTURECHANGED, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
        WM_ENDSESSION, WM_EXITSIZEMOVE, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP,
        WM_NCHITTEST, WM_PAINT, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP,
        WM_SETCURSOR, WM_TIMER, WM_WTSSESSION_CHANGE,
    };

    // Per Section 7.1: Catch any Rust panic at the FFI boundary to prevent immediate process abort
    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LauncherWindowHost;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        // WindowFromPoint in the wheel hook can request a hit test synchronously.
        if msg == WM_NCHITTEST {
            return LRESULT(HTCLIENT as isize);
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

            WM_SETCURSOR => {
                if host.drag_state.is_some() || host.hovered == HitTarget::Grip {
                    if let Ok(hcur) = LoadCursorW(None, IDC_SIZEALL) {
                        let _ = SetCursor(hcur);
                        return LRESULT(1);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // Section 2.2: Double-clicking the grip resets launcher position
            WM_LBUTTONDBLCLK => {
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if host.layout.hit_test(client_x, client_y) == HitTarget::Grip {
                    if host.drag_state.take().is_some() {
                        let _ = ReleaseCapture();
                    }
                    let (x, y, w, h) = host.app.reset_launcher_position();
                    host.apply_window_bounds(x, y, w, h);
                    return LRESULT(0);
                }
                LRESULT(0)
            }

            WM_NCLBUTTONDBLCLK => {
                if wparam.0 == HTCAPTION as usize {
                    let (x, y, w, h) = host.app.reset_launcher_position();
                    host.apply_window_bounds(x, y, w, h);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // Section 2.1: Right-clicking anywhere on the launcher opens our context menu
            WM_NCRBUTTONUP => {
                host.hide_cursor_popup();
                host.hide_tooltip();
                let screen_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let screen_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let has_active_scene = matches!(host.app.state_mgr.state(), SceneState::Active(_));
                if let Some(cmd) = show_context_menu(
                    hwnd,
                    screen_x,
                    screen_y,
                    host.app.config.launcher.orientation,
                    host.app.last_warnings.len(),
                    has_active_scene,
                ) {
                    host.handle_menu_command(cmd);
                }
                LRESULT(0)
            }

            WM_CONTEXTMENU | WM_RBUTTONUP => {
                if host.drag_state.take().is_some() {
                    let _ = ReleaseCapture();
                }
                host.pressed = HitTarget::None;
                host.hide_cursor_popup();
                host.hide_tooltip();
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let has_active_scene = matches!(host.app.state_mgr.state(), SceneState::Active(_));
                if let Some(cmd) = show_context_menu(
                    hwnd,
                    pt.x,
                    pt.y,
                    host.app.config.launcher.orientation,
                    host.app.last_warnings.len(),
                    has_active_scene,
                ) {
                    host.handle_menu_command(cmd);
                }
                LRESULT(0)
            }

            // Section 2.2: Start custom drag only when clicking on the dedicated grip area
            WM_LBUTTONDOWN => {
                host.hide_tooltip();
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let hit = host.layout.hit_test(client_x, client_y);
                match hit {
                    HitTarget::Grip => {
                        if let Some(ref mut ov) = host.overflow_popup {
                            ov.hide();
                        }
                        let mut cursor_pt = POINT::default();
                        let mut win_rc = RECT::default();
                        if GetCursorPos(&mut cursor_pt).is_ok()
                            && GetWindowRect(hwnd, &mut win_rc).is_ok()
                        {
                            let w = (win_rc.right - win_rc.left).max(1) as f64;
                            let h = (win_rc.bottom - win_rc.top).max(1) as f64;
                            let grab_rel_x = ((cursor_pt.x - win_rc.left) as f64 / w).clamp(0.0, 1.0);
                            let grab_rel_y = ((cursor_pt.y - win_rc.top) as f64 / h).clamp(0.0, 1.0);
                            host.drag_state = Some(LauncherDragState {
                                grab_rel_x,
                                grab_rel_y,
                                start_cursor_x: cursor_pt.x,
                                start_cursor_y: cursor_pt.y,
                                moved: false,
                            });
                            let _ = KillTimer(hwnd, TIMER_DISPLAY_DEBOUNCE);
                            let _ = SetCapture(hwnd);
                        }
                    }
                    HitTarget::SceneButton { .. } | HitTarget::OverflowButton => {
                        host.pressed = hit;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    HitTarget::None => {}
                }
                LRESULT(0)
            }

            // Fallback if OS size/move occurs externally
            WM_EXITSIZEMOVE => {
                host.hide_tooltip();
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    let (clamped_x, clamped_y, w, h) =
                        host.app.on_launcher_drag_end(rc.left, rc.top);
                    host.apply_window_bounds(clamped_x, clamped_y, w, h);
                }
                LRESULT(0)
            }

            WM_MOUSEMOVE => {
                if let Some(mut drag) = host.drag_state {
                    let mut cursor_pt = POINT::default();
                    if GetCursorPos(&mut cursor_pt).is_ok() {
                        if !drag.moved
                            && (cursor_pt.x - drag.start_cursor_x).abs() <= 1
                            && (cursor_pt.y - drag.start_cursor_y).abs() <= 1
                        {
                            return LRESULT(0);
                        }
                        drag.moved = true;
                        host.drag_state = Some(drag);
                        host.update_drag_position(
                            cursor_pt.x,
                            cursor_pt.y,
                            drag.grab_rel_x,
                            drag.grab_rel_y,
                        );
                    }
                    return LRESULT(0);
                }

                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                if !host.tracking_mouse_leave {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    host.tracking_mouse_leave = TrackMouseEvent(&mut tme).is_ok();
                    if host.tracking_mouse_leave && !host.app.config.scenes.is_empty() {
                        let _ = host.cursor_input.set_bar_hovered(true);
                    }
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
                let _ = host.cursor_input.set_bar_hovered(false);
                host.bar_wheel = SceneWheel::default();
                if host.drag_state.is_none() {
                    host.hovered = HitTarget::None;
                    host.pressed = HitTarget::None;
                    host.hide_tooltip();
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_MOUSEWHEEL | WM_APP_CURSOR_POPUP_WHEEL => {
                let delta = (wparam.0 >> 16) as i16;
                let mut point = POINT {
                    x: i32::from(lparam.0 as i16),
                    y: i32::from((lparam.0 >> 16) as i16),
                };
                let steps = if msg == WM_APP_CURSOR_POPUP_WHEEL {
                    host.cursor_popup
                        .as_mut()
                        .map_or(0, |popup| popup.wheel_steps(delta, point))
                } else if host.app.config.launcher.show_bar
                    && host.drag_state.is_none()
                    && !host.cursor_popup.as_ref().is_some_and(|popup| popup.visible)
                    && ScreenToClient(hwnd, &mut point).as_bool()
                    && (0..host.layout.width_px).contains(&point.x)
                    && (0..host.layout.height_px).contains(&point.y)
                {
                    host.bar_wheel.steps(delta)
                } else {
                    0
                };
                let source = if msg == WM_APP_CURSOR_POPUP_WHEEL {
                    SwitchSource::PopupWheel
                } else {
                    SwitchSource::BarWheel
                };
                host.cycle_scene(steps, source);
                LRESULT(0)
            }

            WM_CAPTURECHANGED => {
                if let Some(drag) = host.drag_state.take() {
                    if drag.moved {
                        let mut rc = RECT::default();
                        let mut cursor_pt = POINT::default();
                        let anchor = GetCursorPos(&mut cursor_pt)
                            .ok()
                            .map(|_| (cursor_pt.x, cursor_pt.y));
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            let (clamped_x, clamped_y, w, h) =
                                host.app.on_launcher_drag_end_at(rc.left, rc.top, anchor);
                            host.apply_window_bounds(clamped_x, clamped_y, w, h);
                        }
                    }
                }
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                host.hide_tooltip();
                let had_pressed = host.pressed != HitTarget::None;
                host.pressed = HitTarget::None;
                if let Some(drag) = host.drag_state.take() {
                    let _ = ReleaseCapture();
                    if drag.moved {
                        let mut rc = RECT::default();
                        let mut cursor_pt = POINT::default();
                        let anchor = GetCursorPos(&mut cursor_pt)
                            .ok()
                            .map(|_| (cursor_pt.x, cursor_pt.y));
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            let (clamped_x, clamped_y, w, h) =
                                host.app.on_launcher_drag_end_at(rc.left, rc.top, anchor);
                            host.apply_window_bounds(clamped_x, clamped_y, w, h);
                        }
                    }
                    return LRESULT(0);
                }
                if had_pressed {
                    let _ = InvalidateRect(hwnd, None, false);
                }

                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                match host.layout.hit_test(client_x, client_y) {
                    HitTarget::SceneButton { scene_id, .. } => {
                        if let Some(ref mut ov) = host.overflow_popup {
                            ov.hide();
                        }
                        host.trigger_scene_switch(&scene_id, SwitchSource::BarClick);
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
                    host.trigger_scene_switch(&scene_id, SwitchSource::Overflow);
                }
                LRESULT(0)
            }

            WM_APP_WINEVENT => {
                host.app.process_queued_win_events();
                LRESULT(0)
            }

            WM_APP_TRAYICON => {
                let mouse_msg = (lparam.0 & 0xFFFF) as u32;
                if mouse_msg == WM_LBUTTONUP {
                    host.toggle_cursor_popup();
                } else if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_CONTEXTMENU {
                    host.hide_cursor_popup();
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let has_active_scene =
                        matches!(host.app.state_mgr.state(), SceneState::Active(_));
                    if let Some(cmd) = show_context_menu(
                        hwnd,
                        pt.x,
                        pt.y,
                        host.app.config.launcher.orientation,
                        host.app.last_warnings.len(),
                        has_active_scene,
                    ) {
                        host.handle_menu_command(cmd);
                    }
                }
                LRESULT(0)
            }

            WM_APP_TOGGLE_CURSOR_POPUP => {
                if host.app.config.launcher.ctrl_double_tap {
                    host.toggle_cursor_popup();
                }
                LRESULT(0)
            }

            WM_APP_DISMISS_CURSOR_POPUP => {
                host.hide_cursor_popup();
                LRESULT(0)
            }

            WM_APP_CURSOR_POPUP_KEY => {
                let vk = wparam.0 as u16;
                let outcome = host
                    .cursor_popup
                    .as_mut()
                    .map_or(CursorKeyOutcome::Ignored, |p| p.handle_key(vk));
                match outcome {
                    CursorKeyOutcome::Dismiss => host.hide_cursor_popup(),
                    CursorKeyOutcome::Execute => {
                        if let Some(action) =
                            host.cursor_popup.as_mut().and_then(|p| p.pending_action.take())
                        {
                            host.execute_cursor_popup_action(action);
                        }
                    }
                    CursorKeyOutcome::Redrawn | CursorKeyOutcome::Ignored => {}
                }
                LRESULT(0)
            }

            WM_APP_CURSOR_POPUP_ACTION => {
                if let Some(action) = host.cursor_popup.as_mut().and_then(|p| p.pending_action.take()) {
                    host.execute_cursor_popup_action(action);
                }
                LRESULT(0)
            }

            WM_APP_OPEN_SETTINGS => {
                host.open_settings_dialog();
                LRESULT(0)
            }

            WM_SETTINGS_REQUEST => {
                let settings = HWND(wparam.0 as *mut _);
                if SettingsWindow::dispatch(settings, hwnd, &mut host.app) {
                    host.hide_cursor_popup();
                    let (x, y, w, h) = host.app.resolve_initial_or_updated_launcher_rect();
                    host.apply_window_bounds(x, y, w, h);
                    let failed = host.register_configured_hotkeys();
                    SettingsWindow::show_hotkey_conflicts(settings, &failed);
                    host.apply_launcher_settings();
                }
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_HOTKEY => {
                let hk_id = wparam.0 as i32 - HOTKEY_BASE_ID;
                if hk_id >= 0 {
                    if let Some(hk) = host.app.config.hotkeys.get(hk_id as usize) {
                        let scene_id = hk.scene_id.clone();
                        host.trigger_scene_switch(&scene_id, SwitchSource::Hotkey);
                    }
                }
                LRESULT(0)
            }

            // Section 7.2: Aggregate rapid DPI / display / power / session change notifications via one-shot timer
            // when not actively dragging or programmatically applying bounds.
            WM_DPICHANGED | WM_DISPLAYCHANGE | WM_POWERBROADCAST | WM_WTSSESSION_CHANGE => {
                host.hide_cursor_popup();
                if msg == WM_WTSSESSION_CHANGE || msg == WM_POWERBROADCAST {
                    host.cursor_input.reset_gesture();
                }
                if host.drag_state.is_none() && !host.applying_bounds {
                    let _ = KillTimer(hwnd, TIMER_DISPLAY_DEBOUNCE);
                    let _ = SetTimer(hwnd, TIMER_DISPLAY_DEBOUNCE, 200, None);
                }
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
                    pressed: &host.pressed,
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
