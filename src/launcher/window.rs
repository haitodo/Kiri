//! Native Win32 Launcher Runtime Host & Per-Monitor Bar Windows (`window.rs`).
//!
//! Architecture:
//! - `LauncherWindowHost`: singleton coordinator owning a hidden top-level HWND (`KiriLauncherWindowClass`),
//!   `AppController`, system tray icon, global hotkeys, WinEvent hooks, `CursorInput`, and `CursorPopup`.
//! - `LauncherBarWindow`: per-monitor fixed launcher bar HWND (`KiriLauncherBarWindowClass`), owning its
//!   own `LauncherLayout`, `GdiThemeCache`, `OverflowPopup`, `VariantFanHost`, `TopmostGuard`, and
//!   cached `BarSnapshot` so `bar_wndproc` never aliases `&mut LauncherWindowHost`.
//!
//! All Win32 behaviors from Sections 2.1, 2.2, 5.1, 7.1, 7.2, 7.3, and 8.2 are preserved:
//! - `WS_POPUP` with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
//! - `WM_MOUSEACTIVATE` -> `MA_NOACTIVATE` (clicks never steal foreground focus)
//! - `WM_NCHITTEST` -> `HTCLIENT`, with grip dragging handled by the bar
//! - `WM_LBUTTONDBLCLK` / `WM_NCLBUTTONDBLCLK` -> resets bar position without maximizing
//! - `WM_NCRBUTTONUP` / `WM_CONTEXTMENU` -> shows custom context menu even on the grip
//! - `WM_DPICHANGED` / `WM_DISPLAYCHANGE` / `WM_POWERBROADCAST` / `WM_WTSSESSION_CHANGE` ->
//!   debounced via a single-shot timer to re-validate monitor layout and DPI
//! - `WM_QUERYENDSESSION` / `WM_ENDSESSION` -> flushes journal without restoring windows on OS logoff
//! - `std::panic::catch_unwind` at every `extern "system" fn` boundary.

use crate::app::AppController;
use crate::config::{BarDisplayMode, LauncherConfig, Orientation, RectData, Scene};
use crate::diagnostics::{log_error, log_info, log_warn};
use crate::launcher::cursor_popup::{
    CursorKeyOutcome, CursorPopup, PopupAction, WM_APP_CURSOR_POPUP_ACTION,
    WM_APP_CURSOR_POPUP_FAN_STATE,
};
use crate::launcher::input::{
    wheel_scene_index, HitTarget, LauncherLayout, SceneWheel, CORNER_RADIUS_DIP,
};
use crate::launcher::menu::{show_context_menu, MenuCommand};
use crate::launcher::overflow::{OverflowPopup, WM_APP_OVERFLOW_SELECT};
use crate::launcher::paint::{paint_launcher, GdiThemeCache, GdiplusContext, PaintState};
use crate::launcher::topmost_guard::{
    inspect_topmost_status, raise_launcher_to_topmost, GuardDecision, TopmostGuard,
    TOPMOST_DEBOUNCE_MS,
};
use crate::launcher::variant_fan::{FanAxis, FanOutcome, VariantFanHost, HOLD_OPEN_DELAY_MS};
use crate::platform::cursor_input::{
    CursorInput, WM_APP_CANCEL_VARIANT_FAN, WM_APP_CURSOR_POPUP_KEY, WM_APP_CURSOR_POPUP_WHEEL,
    WM_APP_DISMISS_CURSOR_POPUP, WM_APP_SHOW_CURSOR_POPUP,
};
use crate::platform::hooks::{
    take_foreground_change_pending, ForegroundHookGuard, WinEventHookGuard,
    WM_APP_FOREGROUND_CHANGED, WM_APP_WINEVENT,
};
use crate::platform::monitors::{
    clamp_and_snap_in_work_area, dip_to_px, enumerate_connected_monitors, find_monitor_for_point,
    ResolvedLauncherBar,
};
use crate::platform::tray::{TrayIcon, WM_APP_TRAYICON};
use crate::platform::MonitorInfoData;
use crate::scenes::manager::SceneState;
use crate::scenes::switch::{format_switch_warnings, SwitchSource, SwitchWarning};
use crate::settings::{
    window::{LauncherChange, SettingsWindow, WM_SETTINGS_REQUEST},
    SettingsPage,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use windows::core::PWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Controls::{
    TOOLTIPS_CLASSW, TTF_ABSOLUTE, TTF_TRACK, TTM_ADDTOOLW, TTM_TRACKACTIVATE, TTM_TRACKPOSITION,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

/// Window class name used for single-instance discovery (`FindWindowW`).
pub const LAUNCHER_WINDOW_CLASS: &str = "KiriLauncherWindowClass";

/// Message sent by a 2nd process instance to open Settings on the running instance (Section 7.2 & 8.2).
pub const WM_APP_OPEN_SETTINGS: u32 = 0x8000 + 4; // WM_APP + 4
const WM_APP_BAR_EVENT: u32 = 0x8000 + 16; // WM_APP + 16

const TIMER_VERIFY_SWITCH: usize = 101;
const TIMER_DISPLAY_DEBOUNCE: usize = 102;
const TIMER_VARIANT_HOLD: usize = 103;
const TIMER_TOPMOST_CHECK: usize = 104;
const HOTKEY_BASE_ID: i32 = 4000;
const WM_MOUSELEAVE: u32 = 0x02A3;

struct LauncherDragState {
    grab_rel_x: f64,
    grab_rel_y: f64,
    start_cursor_x: i32,
    start_cursor_y: i32,
    moved: bool,
    monitors: Vec<MonitorInfoData>,
}

struct BarSnapshot {
    launcher_cfg: LauncherConfig,
    visible_scenes: Vec<Scene>,
    overflow_scenes: Vec<Scene>,
    active_scene_id: Option<String>,
    scene_state: SceneState,
    last_known_scene_id: Option<String>,
    active_in_overflow: bool,
    warning_count: usize,
}

impl BarSnapshot {
    fn from_app(app: &AppController) -> Self {
        let (visible_scenes, overflow_scenes) = app.config.split_visible_and_overflow();
        let active_scene = app.state_mgr.active_scene_id();
        Self {
            launcher_cfg: app.config.launcher.clone(),
            active_in_overflow: overflow_scenes
                .iter()
                .any(|s| Some(s.id.as_str()) == active_scene),
            visible_scenes,
            overflow_scenes,
            active_scene_id: active_scene.map(str::to_owned),
            scene_state: app.state_mgr.state().clone(),
            last_known_scene_id: app.state_mgr.last_known_scene_id().map(str::to_owned),
            warning_count: app.last_warnings.len(),
        }
    }

    fn compute_layout(&self, dpi: u32) -> LauncherLayout {
        LauncherLayout::compute(
            &self.launcher_cfg,
            &self.visible_scenes,
            !self.overflow_scenes.is_empty(),
            dpi,
        )
    }

    fn has_scenes(&self) -> bool {
        !self.visible_scenes.is_empty() || !self.overflow_scenes.is_empty()
    }

    fn find_scene(&self, scene_id: &str) -> Option<&Scene> {
        self.visible_scenes
            .iter()
            .chain(self.overflow_scenes.iter())
            .find(|s| s.id == scene_id)
    }
}

enum BarEvent {
    SwitchScene {
        bar_hwnd: usize,
        monitor_id: String,
        scene_id: String,
        variant_id: Option<String>,
        source: SwitchSource,
    },
    CycleScene {
        bar_hwnd: usize,
        monitor_id: String,
        steps: i32,
        source: SwitchSource,
    },
    DragEnd {
        bar_hwnd: usize,
        monitor_id: String,
        screen_x: i32,
        screen_y: i32,
        dpi: u32,
        cursor_anchor: Option<(i32, i32)>,
    },
    ResetBarPosition {
        monitor_id: String,
    },
    OpenContextMenu {
        bar_hwnd: usize,
        monitor_id: String,
        screen_x: i32,
        screen_y: i32,
    },
    HoverChanged {
        bar_hwnd: usize,
        hovered: bool,
    },
    FanActiveChanged {
        bar_hwnd: usize,
        active: bool,
    },
    OverflowOpening {
        bar_hwnd: usize,
    },
    DisplayOrDpiChanged,
}

thread_local! {
    static BAR_EVENTS: RefCell<VecDeque<BarEvent>> = const { RefCell::new(VecDeque::new()) };
}

fn push_bar_event(host_hwnd: HWND, event: BarEvent) {
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    BAR_EVENTS.with_borrow_mut(|queue| queue.push_back(event));
    unsafe {
        let _ = PostMessageW(Some(host_hwnd), WM_APP_BAR_EVENT, WPARAM(0), LPARAM(0));
    }
}

#[cfg(windows)]
pub struct LauncherBarWindow {
    pub hwnd: HWND,
    host_hwnd: HWND,
    pub monitor_id: String,
    pub dpi: u32,
    pub work_area: RectData,
    tooltip_hwnd: HWND,
    tooltip_visible: bool,
    tracking_mouse_leave: bool,
    drag_state: Option<LauncherDragState>,
    sync_after_capture: bool,
    applying_bounds: bool,
    layout: LauncherLayout,
    hovered: HitTarget,
    pressed: HitTarget,
    theme: GdiThemeCache,
    overflow_popup: Option<Box<OverflowPopup>>,
    variant_fan: VariantFanHost,
    bar_wheel: SceneWheel,
    topmost_guard: TopmostGuard,
    snapshot: Rc<BarSnapshot>,
}

#[cfg(windows)]
impl Drop for LauncherBarWindow {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            DestroyWindow, KillTimer, SetWindowLongPtrW, GWLP_USERDATA,
        };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TOPMOST_CHECK);
            let _ = KillTimer(Some(self.hwnd), TIMER_VARIANT_HOLD);
            self.hide_tooltip();
            if let Some(ov) = &mut self.overflow_popup {
                ov.hide();
            }
            let _ = self.variant_fan.cancel();
            if !self.tooltip_hwnd.0.is_null() {
                let _ = DestroyWindow(self.tooltip_hwnd);
            }
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(windows)]
impl LauncherBarWindow {
    fn create_and_show(
        host_hwnd: HWND,
        resolved: &ResolvedLauncherBar,
        work_area: RectData,
        snapshot: Rc<BarSnapshot>,
    ) -> Result<Box<Self>, String> {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowLongPtrW, ShowWindow, CS_DBLCLKS,
            CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, SW_SHOWNOACTIVATE, WNDCLASSW,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };

        static REGISTER_BAR_ONCE: Once = Once::new();
        let class_name = w!("KiriLauncherBarWindowClass");
        let dpi = resolved.dpi.max(96);
        let layout = snapshot.compute_layout(dpi);

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_BAR_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
                    lpfnWndProc: Some(bar_wndproc),
                    hInstance: hinstance.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            if snapshot.launcher_cfg.always_on_top {
                ex_style |= WS_EX_TOPMOST;
            }

            let hwnd = CreateWindowExW(
                ex_style,
                class_name,
                w!("KiriBar"),
                WS_POPUP,
                resolved.x,
                resolved.y,
                resolved.width_px,
                resolved.height_px,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW(bar) failed: {}", e))?;

            let theme = GdiThemeCache::new(dpi);
            let overflow_popup = OverflowPopup::create(hwnd, theme.gdiplus.clone()).ok();
            let variant_fan = VariantFanHost::new(theme.gdiplus.clone());

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
                Some(hwnd),
                None,
                Some(hinstance.into()),
                None,
            )
            .unwrap_or_default();

            if !tooltip_hwnd.0.is_null() {
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
                    Some(WPARAM(0)),
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                );
            }

            let mut bar = Box::new(Self {
                hwnd,
                host_hwnd,
                monitor_id: resolved.monitor_id.clone(),
                dpi,
                work_area,
                tooltip_hwnd,
                tooltip_visible: false,
                tracking_mouse_leave: false,
                drag_state: None,
                sync_after_capture: false,
                applying_bounds: false,
                layout,
                hovered: HitTarget::None,
                pressed: HitTarget::None,
                theme,
                overflow_popup,
                variant_fan,
                bar_wheel: SceneWheel::default(),
                topmost_guard: TopmostGuard::new(),
                snapshot,
            });

            let ptr = &mut *bar as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            bar.apply_window_bounds(
                resolved.x,
                resolved.y,
                resolved.width_px,
                resolved.height_px,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if bar.snapshot.launcher_cfg.always_on_top {
                bar.ensure_topmost_if_needed(true);
            }
            Ok(bar)
        }
    }

    fn apply_snapshot(&mut self, snapshot: Rc<BarSnapshot>) {
        self.snapshot = snapshot;
        self.layout = self.snapshot.compute_layout(self.dpi);
        self.invalidate();
    }

    fn invalidate(&self) {
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn hide_popups(&mut self) {
        self.hide_tooltip();
        if let Some(overflow) = &mut self.overflow_popup {
            overflow.hide();
        }
        self.cancel_bar_variant_fan();
    }

    fn apply_window_bounds(&mut self, x: i32, y: i32, w: i32, h: i32) {
        use windows::Win32::Graphics::Gdi::{
            CreateRoundRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, HGDIOBJ,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
        };

        self.theme.ensure_dpi(self.dpi);
        if self.layout.dpi != self.dpi {
            self.layout = self.snapshot.compute_layout(self.dpi);
        }
        let z_hwnd = if self.snapshot.launcher_cfg.always_on_top {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        let diameter = dip_to_px(CORNER_RADIUS_DIP * 2, self.dpi);
        self.applying_bounds = true;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(z_hwnd), x, y, w, h, SWP_NOACTIVATE);
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, diameter, diameter);
            if !rgn.is_invalid() && SetWindowRgn(self.hwnd, Some(rgn), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(rgn.0));
            }
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        self.applying_bounds = false;
    }

    fn update_drag_position(&mut self, cursor_x: i32, cursor_y: i32) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };

        let Some(drag) = &mut self.drag_state else {
            return;
        };
        if !drag.moved
            && (cursor_x - drag.start_cursor_x).abs() <= 1
            && (cursor_y - drag.start_cursor_y).abs() <= 1
        {
            return;
        }
        drag.moved = true;
        if self.snapshot.launcher_cfg.bar_display_mode == BarDisplayMode::Single {
            let Some(mon) = find_monitor_for_point(cursor_x, cursor_y, &drag.monitors) else {
                return;
            };
            if self.monitor_id != mon.monitor_id {
                self.monitor_id.clone_from(&mon.monitor_id);
            }
            self.dpi = mon.dpi.max(96);
            self.work_area = mon.work_area;
        }
        let resize = self.layout.dpi != self.dpi;
        if resize {
            self.layout = self.snapshot.compute_layout(self.dpi);
        }

        let w = self.layout.width_px;
        let h = self.layout.height_px;
        let raw_x = cursor_x - (drag.grab_rel_x * w as f64).round() as i32;
        let raw_y = cursor_y - (drag.grab_rel_y * h as f64).round() as i32;
        let (x, y) = clamp_and_snap_in_work_area(
            raw_x,
            raw_y,
            w,
            h,
            self.work_area,
            self.dpi,
            self.snapshot.launcher_cfg.snap_to_edges,
        );
        if resize {
            self.apply_window_bounds(x, y, w, h);
        } else {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    fn schedule_topmost_check(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

        if !self.snapshot.launcher_cfg.always_on_top
            || !self.snapshot.launcher_cfg.show_bar
            || self.topmost_guard.is_paused()
            || self.drag_state.is_some()
        {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TOPMOST_CHECK);
            let _ = SetTimer(
                Some(self.hwnd),
                TIMER_TOPMOST_CHECK,
                TOPMOST_DEBOUNCE_MS as u32,
                None,
            );
        }
    }

    fn ensure_topmost_if_needed(&mut self, reset_guard: bool) {
        if reset_guard {
            self.topmost_guard.on_user_interaction();
        }
        if !self.snapshot.launcher_cfg.always_on_top
            || !self.snapshot.launcher_cfg.show_bar
            || self.drag_state.is_some()
        {
            return;
        }
        let status = inspect_topmost_status(self.hwnd, std::process::id());
        let now_ms = crate::diagnostics::current_unix_ms();
        match self.topmost_guard.evaluate(now_ms, status) {
            GuardDecision::NoOp => {}
            GuardDecision::RaiseToTopmost => {
                self.applying_bounds = true;
                let ok = raise_launcher_to_topmost(self.hwnd);
                self.applying_bounds = false;
                if ok {
                    log_info(
                        "launcher",
                        &format!(
                            "Re-asserted launcher HWND_TOPMOST on monitor '{}' after obstruction",
                            self.monitor_id
                        ),
                    );
                }
            }
            GuardDecision::PausedEntered {
                blocker_hwnd,
                blocker_class,
                conflicts,
            } => {
                log_warn(
                    "launcher",
                    &format!(
                        "Paused topmost re-assertion on '{}' after {conflicts} consecutive conflicts with '{blocker_class}' (hwnd=0x{blocker_hwnd:X})",
                        self.monitor_id
                    ),
                );
            }
        }
    }

    fn hide_tooltip(&mut self) {
        if self.tooltip_visible && !self.tooltip_hwnd.0.is_null() {
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
                    Some(WPARAM(0)),
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                );
            }
            self.tooltip_visible = false;
        }
    }

    fn update_tooltip(&mut self, target: &HitTarget, screen_x: i32, screen_y: i32) {
        if self.tooltip_hwnd.0.is_null() {
            return;
        }

        let text_opt = match target {
            HitTarget::SceneButton { scene_id, .. } => {
                self.snapshot.find_scene(scene_id).map(|scene| {
                    let base = match scene.preferred_variant() {
                        Some(variant) => format!("{} / {}", scene.name, variant.name),
                        None => scene.name.clone(),
                    };
                    if scene.variants.len() > 1 {
                        format!("{base}（長押し・ドラッグで構成選択）")
                    } else {
                        base
                    }
                })
            }
            HitTarget::OverflowButton => {
                if self.snapshot.active_in_overflow {
                    let active_name = self
                        .snapshot
                        .active_scene_id
                        .as_deref()
                        .and_then(|id| self.snapshot.find_scene(id))
                        .map(|s| s.name.as_str())
                        .unwrap_or("選択中");
                    Some(format!("⋯ ({})", active_name))
                } else {
                    Some("その他のシーン (⋯)".to_string())
                }
            }
            HitTarget::Grip => {
                if self.snapshot.warning_count == 0 {
                    Some("Kiri (ドラッグ移動 / Wクリックでリセット)".to_string())
                } else {
                    Some(format!(
                        "Kiri — ⚠️ 警告 {}件（右クリックメニューから確認）",
                        self.snapshot.warning_count
                    ))
                }
            }
            HitTarget::None => None,
        };

        if let Some(text) = text_opt {
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
                    Some(WPARAM(0)),
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                );
                let off_x = dip_to_px(12, self.dpi);
                let off_y = dip_to_px(22, self.dpi);
                let tx = (screen_x + off_x) as i16 as u16 as u32;
                let ty = (screen_y + off_y) as i16 as u16 as u32;
                let pos = (ty << 16) | tx;
                let _ = SendMessageW(
                    self.tooltip_hwnd,
                    TTM_TRACKPOSITION,
                    Some(WPARAM(0)),
                    Some(LPARAM(pos as isize)),
                );
                if !self.tooltip_visible {
                    let _ = SendMessageW(
                        self.tooltip_hwnd,
                        TTM_TRACKACTIVATE,
                        Some(WPARAM(1)),
                        Some(LPARAM(&mut ti as *mut _ as isize)),
                    );
                    self.tooltip_visible = true;
                }
            }
        } else {
            self.hide_tooltip();
        }
    }

    fn cancel_bar_variant_fan(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows::Win32::UI::WindowsAndMessaging::KillTimer;

        if self.variant_fan.is_active() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_VARIANT_HOLD);
                let _ = ReleaseCapture();
            }
            let _ = self.variant_fan.cancel();
            push_bar_event(
                self.host_hwnd,
                BarEvent::FanActiveChanged {
                    bar_hwnd: self.hwnd.0 as usize,
                    active: false,
                },
            );
            self.pressed = HitTarget::None;
            self.invalidate();
        }
    }
}

#[cfg(windows)]
pub struct LauncherWindowHost {
    pub hwnd: HWND,
    pub app: AppController,
    pub bars: Vec<Box<LauncherBarWindow>>,
    cursor_input: CursorInput,
    cursor_popup: Option<Box<CursorPopup>>,
    popup_gdiplus: GdiplusContext,
    pub tray: Option<TrayIcon>,
    pub win_event_hook: Option<WinEventHookGuard>,
    pub foreground_hook: Option<ForegroundHookGuard>,
    pub registered_hotkey_count: usize,
}

#[cfg(windows)]
impl Drop for LauncherWindowHost {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            DestroyWindow, KillTimer, SetWindowLongPtrW, GWLP_USERDATA,
        };
        self.bars.clear();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_VERIFY_SWITCH);
            let _ = KillTimer(Some(self.hwnd), TIMER_DISPLAY_DEBOUNCE);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(windows)]
impl LauncherWindowHost {
    pub fn create_and_show(app: AppController) -> Result<Box<Self>, String> {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::RemoteDesktop::{
            WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, RegisterClassW, SetWindowLongPtrW, GWLP_USERDATA, WNDCLASSW,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        };

        static REGISTER_HOST_ONCE: Once = Once::new();
        let class_name = w!("KiriLauncherWindowClass");

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_HOST_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(host_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Kiri"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW(host) failed: {}", e))?;

            let popup_gdiplus = GdiplusContext::init();
            let tray = Some(TrayIcon::new(hwnd));
            let win_event_hook = WinEventHookGuard::install(hwnd);

            let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);

            let mut host = Box::new(Self {
                hwnd,
                app,
                bars: Vec::new(),
                cursor_input: CursorInput::new(hwnd),
                cursor_popup: None,
                popup_gdiplus,
                tray,
                win_event_hook,
                foreground_hook: None,
                registered_hotkey_count: 0,
            });

            let ptr = &mut *host as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            host.register_configured_hotkeys();
            host.check_startup_recovery_prompt();
            host.sync_bars();
            host.apply_runtime_settings();
            if host.app.config_mgr.is_read_only() {
                host.notify_tray(
                    "Kiri - 読み取り専用モード",
                    "新しいバージョンの config.json が読み込まれたため、設定の自動保存は無効です。",
                    true,
                );
            }
            log_info(
                "startup",
                &format!("Launcher ready with {} bar(s)", host.bars.len()),
            );
            Ok(host)
        }
    }

    fn sync_bars(&mut self) {
        let monitors = self.app.sys.monitors();
        let desired = self.app.resolve_launcher_bars(&monitors);
        let snapshot = Rc::new(BarSnapshot::from_app(&self.app));

        let work_area_for = |monitor_id: &str| {
            monitors
                .iter()
                .find(|m| m.monitor_id == monitor_id)
                .map(|m| m.work_area)
                .unwrap_or(RectData {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1040,
                })
        };

        let mut existing = std::mem::take(&mut self.bars);
        let mut next_bars = Vec::with_capacity(desired.len());

        for target in desired {
            let work_area = work_area_for(&target.monitor_id);
            let existing_index = existing
                .iter()
                .position(|b| b.monitor_id == target.monitor_id)
                .or_else(|| {
                    // Reuse the single bar when its monitor changes or disconnects.
                    (existing.len() == 1 && snapshot.launcher_cfg.bar_display_mode == BarDisplayMode::Single)
                        .then_some(0)
                });
            if let Some(pos) = existing_index {
                let mut bar = existing.remove(pos);
                bar.monitor_id = target.monitor_id;
                bar.dpi = target.dpi.max(96);
                bar.work_area = work_area;
                bar.apply_snapshot(snapshot.clone());
                bar.apply_window_bounds(target.x, target.y, target.width_px, target.height_px);
                if snapshot.launcher_cfg.always_on_top {
                    bar.ensure_topmost_if_needed(true);
                }
                next_bars.push(bar);
            } else {
                match LauncherBarWindow::create_and_show(
                    self.hwnd,
                    &target,
                    work_area,
                    snapshot.clone(),
                ) {
                    Ok(bar) => next_bars.push(bar),
                    Err(e) => log_error("startup", &e),
                }
            }
        }

        // Remaining bars in `existing` are dropped here (closing their HWNDs).
        drop(existing);
        self.bars = next_bars;
        if self.bars.is_empty() {
            let _ = self.cursor_input.clear_bar_hover();
            let _ = self.cursor_input.set_bar_fan_active(false);
        }
        self.sync_foreground_hook();
    }

    fn refresh_all_bars_snapshot(&mut self) {
        if self.bars.is_empty() {
            return;
        }
        let snapshot = Rc::new(BarSnapshot::from_app(&self.app));
        for bar in &mut self.bars {
            bar.apply_snapshot(snapshot.clone());
        }
    }

    fn hide_other_bar_popups(&mut self, except_bar_hwnd: Option<usize>) {
        for bar in &mut self.bars {
            if Some(bar.hwnd.0 as usize) != except_bar_hwnd {
                bar.hide_popups();
            }
        }
    }

    fn any_bar_dragging(&self) -> bool {
        self.bars.iter().any(|b| b.drag_state.is_some())
    }

    fn update_display_layout(&mut self) {
        if !self.any_bar_dragging() {
            self.sync_bars();
            for bar in &mut self.bars {
                bar.topmost_guard.on_user_interaction();
            }
            return;
        }

        // Keep the captured HWND under the pointer. Reconcile all bars after capture ends.
        let monitors = self.app.sys.monitors();
        for bar in &mut self.bars {
            let Some(drag) = &mut bar.drag_state else {
                continue;
            };
            if bar.snapshot.launcher_cfg.bar_display_mode == BarDisplayMode::Single {
                drag.monitors.clone_from(&monitors);
            } else if let Some(mon) = monitors.iter().find(|m| m.monitor_id == bar.monitor_id) {
                bar.work_area = mon.work_area;
                bar.dpi = mon.dpi.max(96);
            }
            bar.sync_after_capture = true;
        }
    }

    fn dialog_owner_hwnd(&self, preferred_bar_hwnd: Option<usize>) -> HWND {
        if let Some(raw) = preferred_bar_hwnd {
            if let Some(bar) = self.bars.iter().find(|b| b.hwnd.0 as usize == raw) {
                return bar.hwnd;
            }
        }
        self.bars.first().map_or(self.hwnd, |b| b.hwnd)
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
                Some(self.hwnd),
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
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_BASE_ID + i as i32);
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
                if RegisterHotKey(Some(self.hwnd), id, HOT_KEY_MODIFIERS(hk.modifiers), hk.vk).is_err() {
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

    fn sync_foreground_hook(&mut self) {
        let should_hook = self.app.config.launcher.always_on_top
            && self.app.config.launcher.show_bar
            && !self.bars.is_empty();
        if should_hook {
            if self.foreground_hook.is_none() {
                self.foreground_hook = ForegroundHookGuard::install(self.hwnd);
            }
        } else {
            self.foreground_hook = None;
        }
    }

    fn apply_runtime_settings(&mut self) {
        if let Err(error) = self
            .cursor_input
            .set_ctrl_enabled(self.app.config.launcher.ctrl_double_tap)
        {
            log_warn("hotkey", &format!("Could not enable Ctrl double-tap: {error}"));
        }
        if !self.app.config.launcher.show_bar {
            let _ = self.cursor_input.clear_bar_hover();
            let _ = self.cursor_input.set_bar_fan_active(false);
        }
        self.sync_foreground_hook();
    }

    fn toggle_cursor_popup(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

        self.cursor_input.interrupt_gesture();
        if self.cursor_popup.as_ref().is_some_and(|popup| popup.visible) {
            self.hide_cursor_popup();
            return;
        }
        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_ok() {
            self.show_cursor_popup_at(cursor);
        }
    }

    fn show_cursor_popup_at(&mut self, cursor: POINT) {
        if self.any_bar_dragging() {
            return;
        }
        let monitors = self.app.sys.monitors();
        let Some(monitor) = find_monitor_for_point(cursor.x, cursor.y, &monitors) else {
            return;
        };
        self.hide_other_bar_popups(None);
        if self.cursor_popup.is_none() {
            match CursorPopup::create(self.hwnd, self.popup_gdiplus.clone()) {
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

    fn trigger_scene_switch(
        &mut self,
        scene_id: &str,
        source: SwitchSource,
        source_monitor_id: Option<&str>,
        bar_hwnd: Option<usize>,
    ) {
        self.hide_cursor_popup();
        self.switch_scene(scene_id, None, source, source_monitor_id, bar_hwnd);
    }

    fn cycle_scene(
        &mut self,
        steps: i32,
        source: SwitchSource,
        source_monitor_id: Option<&str>,
        bar_hwnd: Option<usize>,
    ) {
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
        self.hide_other_bar_popups(None);
        self.switch_scene(&scene_id, None, source, source_monitor_id, bar_hwnd);
        if let Some(popup) = &mut self.cursor_popup {
            popup.update_active_scene(self.app.state_mgr.active_scene_id());
        }
    }

    fn switch_scene(
        &mut self,
        scene_id: &str,
        variant: Option<&str>,
        source: SwitchSource,
        source_monitor_id: Option<&str>,
        bar_hwnd: Option<usize>,
    ) {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_ICONINFORMATION, MB_TOPMOST, MB_YESNO,
        };

        let mut outcome =
            self.app
                .switch_to_scene_from_monitor(scene_id, variant, source, source_monitor_id);

        if self
            .app
            .config
            .scene(scene_id)
            .is_some_and(|scene| scene.target.is_none())
            && outcome
                .warnings
                .contains(&SwitchWarning::TargetMonitorNotConfigured)
        {
            self.hide_cursor_popup();
            let owner = self.dialog_owner_hwnd(bar_hwnd);
            unsafe {
                let ans = MessageBoxW(
                    Some(owner),
                    w!("制御対象モニターがまだ設定されていません。\n現在ランチャーがあるモニターを制御対象モニターとして設定しますか？"),
                    w!("Kiri - 初回モニター設定"),
                    MB_YESNO | MB_ICONINFORMATION | MB_TOPMOST,
                );
                if ans == IDYES && !self.app.launcher_monitor_id.is_empty() {
                    self.app.config.target.monitor_id = Some(self.app.launcher_monitor_id.clone());
                    let _ = self.app.config_mgr.save(&self.app.config);
                    outcome = self.app.switch_to_scene_from_monitor(
                        scene_id,
                        variant,
                        source,
                        source_monitor_id,
                    );
                } else {
                    self.refresh_all_bars_snapshot();
                    return;
                }
            }
        }

        if outcome.warnings.contains(&SwitchWarning::RecoveryJournalWriteFailed) {
            let title = if outcome.switched || outcome.was_noop_same_scene {
                "復旧記録を更新できませんでした"
            } else {
                "シーンを切り替えられませんでした"
            };
            self.notify_tray(title, "復旧記録の保存先の空き容量とアクセス権を確認してください。", true);
        }
        if !outcome.was_noop_same_scene || !outcome.warnings.is_empty() {
            if outcome.verification.is_some() {
                self.schedule_switch_verification();
            }
            self.refresh_all_bars_snapshot();
        }
        for bar in &mut self.bars {
            bar.ensure_topmost_if_needed(true);
        }
    }

    fn schedule_switch_verification(&self) {
        use windows::Win32::Foundation::GetLastError;
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_VERIFY_SWITCH);
            if SetTimer(Some(self.hwnd), TIMER_VERIFY_SWITCH, 500, None) == 0 {
                let error = GetLastError().0;
                if let Some(pending) = &self.app.pending_verification {
                    pending.record_timer_failure(error);
                }
            }
        }
    }

    pub fn handle_menu_command(
        &mut self,
        cmd: MenuCommand,
        source_bar: Option<(usize, String)>,
    ) {
        use windows::core::{w, HSTRING};
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, PostQuitMessage, IDYES, MB_ICONWARNING, MB_TOPMOST, MB_YESNO,
        };

        self.hide_other_bar_popups(None);
        let owner = self.dialog_owner_hwnd(source_bar.as_ref().map(|(h, _)| *h));

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
                        Some(owner),
                        &body_w,
                        w!("Kiri - 最新の切替警告"),
                        MB_YESNO | MB_ICONWARNING | MB_TOPMOST,
                    )
                };
                if ans == IDYES {
                    self.app.last_warnings.clear();
                    self.refresh_all_bars_snapshot();
                }
            }
            MenuCommand::OpenCursorPopup => self.toggle_cursor_popup(),
            MenuCommand::OpenSettings => {
                self.open_settings_dialog();
            }
            MenuCommand::ToggleOrientation => {
                self.app.toggle_orientation_and_save();
                self.sync_bars();
            }
            MenuCommand::ResetPosition => {
                let mon_id = source_bar.as_ref().map(|(_, m)| m.as_str());
                self.app.reset_bar_position(mon_id);
                self.sync_bars();
            }
            MenuCommand::HideBarOnMonitor => {
                if let Some((_, mon_id)) = source_bar {
                    if self.app.hide_bar_on_monitor(&mon_id) {
                        self.sync_bars();
                    }
                }
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
                if self.app.state_mgr.active_scene_id().is_none() {
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
                self.refresh_all_bars_snapshot();
            }
            MenuCommand::Exit => {
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
        match action {
            PopupAction::SwitchScene(scene_id) => {
                self.trigger_scene_switch(&scene_id, SwitchSource::PopupClick, None, None)
            }
            PopupAction::SwitchSceneVariant(scene_id, variant_id) => {
                self.hide_cursor_popup();
                self.switch_scene(
                    &scene_id,
                    Some(&variant_id),
                    SwitchSource::PopupClick,
                    None,
                    None,
                );
            }
            PopupAction::OpenShortcut(shortcut) => {
                self.hide_cursor_popup();
                super::shortcuts::open(shortcut);
            }
            PopupAction::OpenSettings => {
                self.hide_cursor_popup();
                self.show_settings(SettingsPage::Shortcuts);
            }
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

    fn drain_and_handle_bar_events(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::SetTimer;

        let events: Vec<BarEvent> = BAR_EVENTS.with_borrow_mut(|q| q.drain(..).collect());
        for event in events {
            match event {
                BarEvent::SwitchScene {
                    bar_hwnd,
                    monitor_id,
                    scene_id,
                    variant_id,
                    source,
                } => {
                    self.hide_other_bar_popups(Some(bar_hwnd));
                    self.hide_cursor_popup();
                    self.switch_scene(
                        &scene_id,
                        variant_id.as_deref(),
                        source,
                        Some(&monitor_id),
                        Some(bar_hwnd),
                    );
                }
                BarEvent::CycleScene {
                    bar_hwnd,
                    monitor_id,
                    steps,
                    source,
                } => {
                    self.cycle_scene(steps, source, Some(&monitor_id), Some(bar_hwnd));
                }
                BarEvent::DragEnd {
                    bar_hwnd,
                    monitor_id,
                    screen_x,
                    screen_y,
                    dpi,
                    cursor_anchor,
                } => {
                    if let Some(resolved) = self.app.on_bar_drag_end(
                        &monitor_id,
                        screen_x,
                        screen_y,
                        dpi,
                        cursor_anchor,
                    ) {
                        if let Some(bar) = self.bars.iter_mut().find(|b| b.hwnd.0 as usize == bar_hwnd) {
                            let monitors = self.app.sys.monitors();
                            if let Some(mon) = monitors
                                .iter()
                                .find(|m| m.monitor_id == resolved.monitor_id)
                            {
                                bar.work_area = mon.work_area;
                            }
                            bar.monitor_id = resolved.monitor_id;
                            bar.dpi = resolved.dpi;
                            bar.apply_window_bounds(
                                resolved.x,
                                resolved.y,
                                resolved.width_px,
                                resolved.height_px,
                            );
                        }
                    } else {
                        self.sync_bars();
                    }
                }
                BarEvent::ResetBarPosition { monitor_id } => {
                    self.app.reset_bar_position(Some(&monitor_id));
                    self.sync_bars();
                }
                BarEvent::OpenContextMenu {
                    bar_hwnd,
                    monitor_id,
                    screen_x,
                    screen_y,
                } => {
                    self.hide_cursor_popup();
                    self.hide_other_bar_popups(Some(bar_hwnd));
                    let owner = self.dialog_owner_hwnd(Some(bar_hwnd));
                    let has_active_scene = self.app.state_mgr.active_scene_id().is_some();
                    let can_hide = self.bars.len() > 1;
                    if let Some(cmd) = show_context_menu(
                        owner,
                        screen_x,
                        screen_y,
                        self.app.config.launcher.orientation,
                        self.app.last_warnings.len(),
                        has_active_scene,
                        can_hide,
                    ) {
                        self.handle_menu_command(cmd, Some((bar_hwnd, monitor_id)));
                    }
                }
                BarEvent::HoverChanged { bar_hwnd, hovered } => {
                    if let Some(bar) = self.bars.iter().find(|b| b.hwnd.0 as usize == bar_hwnd) {
                        let effective_hover = hovered
                            && self.app.config.launcher.show_bar
                            && !self.app.config.scenes.is_empty();
                        let _ = self
                            .cursor_input
                            .set_bar_hovered_hwnd(bar.hwnd, effective_hover);
                    }
                }
                BarEvent::FanActiveChanged { bar_hwnd, active } => {
                    if active {
                        self.hide_other_bar_popups(Some(bar_hwnd));
                        self.hide_cursor_popup();
                    }
                    let _ = self.cursor_input.set_bar_fan_active(active);
                }
                BarEvent::OverflowOpening { bar_hwnd } => {
                    self.hide_other_bar_popups(Some(bar_hwnd));
                    self.hide_cursor_popup();
                }
                BarEvent::DisplayOrDpiChanged => {
                    unsafe {
                        let _ = SetTimer(Some(self.hwnd), TIMER_DISPLAY_DEBOUNCE, 200, None);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetCursorPos, GetWindowLongPtrW, KillTimer, SetTimer, GWLP_USERDATA,
        WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONUP,
        SPI_SETWORKAREA, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE,
        WM_TIMER, WM_WTSSESSION_CHANGE,
    };

    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LauncherWindowHost;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let host = &mut *ptr;

        if let Some(ref mut tray) = host.tray {
            let tb_msg = tray.taskbar_created_message_id();
            if tb_msg != 0 && msg == tb_msg {
                log_info("tray", "TaskbarCreated received; re-registering tray icon");
                tray.add_or_readd();
                return LRESULT(0);
            }
        }

        match msg {
            WM_APP_BAR_EVENT => {
                host.drain_and_handle_bar_events();
                LRESULT(0)
            }

            WM_APP_WINEVENT => {
                host.app.process_queued_win_events();
                LRESULT(0)
            }

            WM_APP_FOREGROUND_CHANGED => {
                if take_foreground_change_pending() {
                    for bar in &host.bars {
                        bar.schedule_topmost_check();
                    }
                }
                LRESULT(0)
            }

            WM_APP_TRAYICON => {
                let mouse_msg = (lparam.0 & 0xFFFF) as u32;
                if mouse_msg == WM_LBUTTONUP {
                    host.toggle_cursor_popup();
                } else if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_CONTEXTMENU {
                    host.hide_cursor_popup();
                    host.hide_other_bar_popups(None);
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let has_active_scene = host.app.state_mgr.active_scene_id().is_some();
                    if let Some(cmd) = show_context_menu(
                        hwnd,
                        pt.x,
                        pt.y,
                        host.app.config.launcher.orientation,
                        host.app.last_warnings.len(),
                        has_active_scene,
                        false,
                    ) {
                        host.handle_menu_command(cmd, None);
                    }
                }
                LRESULT(0)
            }

            WM_APP_SHOW_CURSOR_POPUP => {
                if host.app.config.launcher.ctrl_double_tap {
                    host.show_cursor_popup_at(POINT {
                        x: wparam.0 as i32,
                        y: lparam.0 as i32,
                    });
                }
                LRESULT(0)
            }

            WM_APP_DISMISS_CURSOR_POPUP => {
                host.hide_cursor_popup();
                LRESULT(0)
            }

            WM_APP_CURSOR_POPUP_WHEEL => {
                let delta = (wparam.0 >> 16) as i16;
                let point = POINT {
                    x: i32::from(lparam.0 as i16),
                    y: i32::from((lparam.0 >> 16) as i16),
                };
                let steps = host
                    .cursor_popup
                    .as_mut()
                    .map_or(0, |popup| popup.wheel_steps(delta, point));
                host.cycle_scene(steps, SwitchSource::PopupWheel, None, None);
                LRESULT(0)
            }

            WM_APP_CURSOR_POPUP_KEY => {
                let vk = wparam.0 as u16;
                let outcome = host
                    .cursor_popup
                    .as_mut()
                    .map_or(CursorKeyOutcome::Ignored, |p| p.handle_key(vk));
                let fan_active = host
                    .cursor_popup
                    .as_ref()
                    .is_some_and(|p| p.is_fan_active());
                host.cursor_input.set_popup_fan_active(fan_active);
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

            WM_APP_CURSOR_POPUP_FAN_STATE => {
                host.cursor_input.set_popup_fan_active(wparam.0 != 0);
                LRESULT(0)
            }

            WM_APP_CANCEL_VARIANT_FAN => {
                for bar in &mut host.bars {
                    bar.cancel_bar_variant_fan();
                }
                LRESULT(0)
            }

            WM_APP_CURSOR_POPUP_ACTION => {
                if let Some(action) = host
                    .cursor_popup
                    .as_mut()
                    .and_then(|p| p.pending_action.take())
                {
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
                match SettingsWindow::dispatch(settings, hwnd, &mut host.app) {
                    LauncherChange::SavedSettings => {
                        host.hide_cursor_popup();
                        host.hide_other_bar_popups(None);
                        host.sync_bars();
                        let failed = host.register_configured_hotkeys();
                        SettingsWindow::show_hotkey_conflicts(settings, &failed);
                        host.apply_runtime_settings();
                    }
                    LauncherChange::SceneState => host.refresh_all_bars_snapshot(),
                    LauncherChange::None => {}
                }
                LRESULT(0)
            }

            WM_HOTKEY => {
                let hk_id = wparam.0 as i32 - HOTKEY_BASE_ID;
                if hk_id >= 0 {
                    if let Some(hk) = host.app.config.hotkeys.get(hk_id as usize) {
                        let scene_id = hk.scene_id.clone();
                        host.trigger_scene_switch(&scene_id, SwitchSource::Hotkey, None, None);
                    }
                }
                LRESULT(0)
            }

            WM_DISPLAYCHANGE | WM_POWERBROADCAST | WM_WTSSESSION_CHANGE => {
                host.hide_other_bar_popups(None);
                host.hide_cursor_popup();
                if msg == WM_WTSSESSION_CHANGE || msg == WM_POWERBROADCAST {
                    host.cursor_input.reset_gesture();
                }
                let _ = SetTimer(Some(hwnd), TIMER_DISPLAY_DEBOUNCE, 200, None);
                LRESULT(0)
            }
            WM_SETTINGCHANGE if wparam.0 == SPI_SETWORKAREA.0 as usize => {
                let _ = SetTimer(Some(hwnd), TIMER_DISPLAY_DEBOUNCE, 200, None);
                LRESULT(0)
            }

            WM_TIMER => {
                match wparam.0 {
                    TIMER_VERIFY_SWITCH => {
                        let _ = KillTimer(Some(hwnd), TIMER_VERIFY_SWITCH);
                        let new_warnings = host.app.run_post_switch_verification();
                        if !new_warnings.is_empty() {
                            host.refresh_all_bars_snapshot();
                        }
                        for bar in &mut host.bars {
                            bar.ensure_topmost_if_needed(false);
                        }
                    }
                    TIMER_DISPLAY_DEBOUNCE => {
                        let _ = KillTimer(Some(hwnd), TIMER_DISPLAY_DEBOUNCE);
                        host.update_display_layout();
                    }
                    _ => {}
                }
                LRESULT(0)
            }

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
            log_error("wndproc", "Caught panic across FFI boundary in host_wndproc");
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn bar_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, ClientToScreen, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
        SetCursor, SetTimer, GWLP_USERDATA, HTCAPTION, HTCLIENT, IDC_SIZEALL, MA_NOACTIVATE,
        SWP_NOZORDER, WINDOWPOS, WM_CAPTURECHANGED, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED,
        WM_EXITSIZEMOVE, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCLBUTTONDBLCLK, WM_NCHITTEST, WM_NCRBUTTONUP, WM_PAINT,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WM_WINDOWPOSCHANGED,
    };

    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LauncherBarWindow;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_NCHITTEST {
            return LRESULT(HTCLIENT as isize);
        }
        let bar = &mut *ptr;

        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

            WM_SETCURSOR => {
                if bar.drag_state.is_some() || bar.hovered == HitTarget::Grip {
                    if let Ok(hcur) = LoadCursorW(None, IDC_SIZEALL) {
                        let _ = SetCursor(Some(hcur));
                        return LRESULT(1);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_LBUTTONDBLCLK => {
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if bar.layout.hit_test(client_x, client_y) == HitTarget::Grip {
                    if bar.drag_state.take().is_some() {
                        let _ = ReleaseCapture();
                    }
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::ResetBarPosition {
                            monitor_id: bar.monitor_id.clone(),
                        },
                    );
                    return LRESULT(0);
                }
                LRESULT(0)
            }

            WM_NCLBUTTONDBLCLK => {
                if wparam.0 == HTCAPTION as usize {
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::ResetBarPosition {
                            monitor_id: bar.monitor_id.clone(),
                        },
                    );
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_NCRBUTTONUP => {
                bar.hide_tooltip();
                if let Some(ref mut ov) = bar.overflow_popup {
                    ov.hide();
                }
                let screen_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let screen_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                push_bar_event(
                    bar.host_hwnd,
                    BarEvent::OpenContextMenu {
                        bar_hwnd: hwnd.0 as usize,
                        monitor_id: bar.monitor_id.clone(),
                        screen_x,
                        screen_y,
                    },
                );
                LRESULT(0)
            }

            WM_RBUTTONDOWN => {
                if bar.variant_fan.is_active() {
                    bar.cancel_bar_variant_fan();
                }
                LRESULT(0)
            }

            WM_CONTEXTMENU | WM_RBUTTONUP => {
                if bar.variant_fan.is_active() {
                    bar.cancel_bar_variant_fan();
                    return LRESULT(0);
                }
                if bar.drag_state.take().is_some() {
                    let _ = ReleaseCapture();
                }
                bar.pressed = HitTarget::None;
                bar.hide_tooltip();
                if let Some(ref mut ov) = bar.overflow_popup {
                    ov.hide();
                }
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                push_bar_event(
                    bar.host_hwnd,
                    BarEvent::OpenContextMenu {
                        bar_hwnd: hwnd.0 as usize,
                        monitor_id: bar.monitor_id.clone(),
                        screen_x: pt.x,
                        screen_y: pt.y,
                    },
                );
                LRESULT(0)
            }

            WM_LBUTTONDOWN => {
                bar.topmost_guard.on_user_interaction();
                bar.hide_tooltip();
                if bar.variant_fan.is_sticky() {
                    let mut cursor_pt = POINT::default();
                    if GetCursorPos(&mut cursor_pt).is_ok()
                        && matches!(
                            bar.variant_fan.on_down(cursor_pt.x, cursor_pt.y),
                            FanOutcome::Cancelled
                        )
                    {
                        let _ = ReleaseCapture();
                        push_bar_event(
                            bar.host_hwnd,
                            BarEvent::FanActiveChanged {
                                bar_hwnd: hwnd.0 as usize,
                                active: false,
                            },
                        );
                        bar.pressed = HitTarget::None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                }
                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let hit = bar.layout.hit_test(client_x, client_y);
                match hit {
                    HitTarget::Grip => {
                        if let Some(ref mut ov) = bar.overflow_popup {
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
                            bar.drag_state = Some(LauncherDragState {
                                grab_rel_x,
                                grab_rel_y,
                                start_cursor_x: cursor_pt.x,
                                start_cursor_y: cursor_pt.y,
                                moved: false,
                                monitors: if bar.snapshot.launcher_cfg.bar_display_mode == BarDisplayMode::Single {
                                    enumerate_connected_monitors()
                                } else {
                                    Vec::new()
                                },
                            });
                            let _ = SetCapture(hwnd);
                        }
                    }
                    HitTarget::SceneButton { index, .. } => {
                        bar.pressed = hit;
                        if let Some(slot) = bar.layout.scene_buttons.get(index) {
                            if slot.scene.variants.len() > 1 {
                                let mut cursor_pt = POINT::default();
                                let mut win_rc = RECT::default();
                                if GetCursorPos(&mut cursor_pt).is_ok()
                                    && GetWindowRect(hwnd, &mut win_rc).is_ok()
                                {
                                    let anchor = RectData {
                                        left: win_rc.left + slot.rect.left,
                                        top: win_rc.top + slot.rect.top,
                                        right: win_rc.left + slot.rect.right,
                                        bottom: win_rc.top + slot.rect.bottom,
                                    };
                                    let axis = match bar.snapshot.launcher_cfg.orientation {
                                        Orientation::Horizontal => FanAxis::Vertical,
                                        Orientation::Vertical => FanAxis::Horizontal,
                                    };
                                    let scene = slot.scene.clone();
                                    if bar.variant_fan.begin_press(
                                        &scene,
                                        anchor,
                                        bar.work_area,
                                        axis,
                                        bar.dpi,
                                        cursor_pt.x,
                                        cursor_pt.y,
                                    ) {
                                        let _ = SetCapture(hwnd);
                                        let _ = SetTimer(
                                            Some(hwnd),
                                            TIMER_VARIANT_HOLD,
                                            HOLD_OPEN_DELAY_MS,
                                            None,
                                        );
                                    }
                                }
                            }
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    HitTarget::OverflowButton => {
                        bar.pressed = hit;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    HitTarget::None => {}
                }
                LRESULT(0)
            }

            WM_EXITSIZEMOVE => {
                bar.hide_tooltip();
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::DragEnd {
                            bar_hwnd: hwnd.0 as usize,
                            monitor_id: bar.monitor_id.clone(),
                            screen_x: rc.left,
                            screen_y: rc.top,
                            dpi: bar.dpi,
                            cursor_anchor: None,
                        },
                    );
                }
                LRESULT(0)
            }

            WM_MOUSEMOVE => {
                if bar.variant_fan.is_active() {
                    let mut cursor_pt = POINT::default();
                    if GetCursorPos(&mut cursor_pt).is_ok()
                        && matches!(
                            bar.variant_fan.on_move(cursor_pt.x, cursor_pt.y),
                            FanOutcome::Opened
                        )
                    {
                        let _ = KillTimer(Some(hwnd), TIMER_VARIANT_HOLD);
                        push_bar_event(
                            bar.host_hwnd,
                            BarEvent::FanActiveChanged {
                                bar_hwnd: hwnd.0 as usize,
                                active: true,
                            },
                        );
                    }
                    return LRESULT(0);
                }
                if bar.drag_state.is_some() {
                    let mut cursor_pt = POINT::default();
                    if GetCursorPos(&mut cursor_pt).is_ok() {
                        bar.update_drag_position(cursor_pt.x, cursor_pt.y);
                    }
                    return LRESULT(0);
                }

                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                if !bar.tracking_mouse_leave {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    bar.tracking_mouse_leave = TrackMouseEvent(&mut tme).is_ok();
                    if bar.tracking_mouse_leave && bar.snapshot.has_scenes() {
                        push_bar_event(
                            bar.host_hwnd,
                            BarEvent::HoverChanged {
                                bar_hwnd: hwnd.0 as usize,
                                hovered: true,
                            },
                        );
                    }
                }

                let new_target = bar.layout.hit_test(client_x, client_y);
                if new_target != bar.hovered {
                    bar.hovered = new_target.clone();
                    let mut pt = POINT {
                        x: client_x,
                        y: client_y,
                    };
                    let _ = ClientToScreen(hwnd, &mut pt);
                    bar.update_tooltip(&new_target, pt.x, pt.y);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_MOUSELEAVE => {
                bar.tracking_mouse_leave = false;
                push_bar_event(
                    bar.host_hwnd,
                    BarEvent::HoverChanged {
                        bar_hwnd: hwnd.0 as usize,
                        hovered: false,
                    },
                );
                bar.bar_wheel = SceneWheel::default();
                if bar.drag_state.is_none() && !bar.variant_fan.is_active() {
                    bar.hovered = HitTarget::None;
                    bar.pressed = HitTarget::None;
                    bar.hide_tooltip();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_MOUSEWHEEL => {
                let delta = (wparam.0 >> 16) as i16;
                let mut point = POINT {
                    x: i32::from(lparam.0 as i16),
                    y: i32::from((lparam.0 >> 16) as i16),
                };
                let steps = if bar.snapshot.launcher_cfg.show_bar
                    && bar.drag_state.is_none()
                    && ScreenToClient(hwnd, &mut point).as_bool()
                    && (0..bar.layout.width_px).contains(&point.x)
                    && (0..bar.layout.height_px).contains(&point.y)
                {
                    bar.bar_wheel.steps(delta)
                } else {
                    0
                };
                if steps != 0 {
                    bar.hide_tooltip();
                    if let Some(ref mut ov) = bar.overflow_popup {
                        ov.hide();
                    }
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::CycleScene {
                            bar_hwnd: hwnd.0 as usize,
                            monitor_id: bar.monitor_id.clone(),
                            steps,
                            source: SwitchSource::BarWheel,
                        },
                    );
                }
                LRESULT(0)
            }

            WM_CAPTURECHANGED => {
                if std::mem::take(&mut bar.sync_after_capture) {
                    push_bar_event(bar.host_hwnd, BarEvent::DisplayOrDpiChanged);
                }
                if bar.variant_fan.is_active() {
                    let _ = KillTimer(Some(hwnd), TIMER_VARIANT_HOLD);
                    let _ = bar.variant_fan.cancel();
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::FanActiveChanged {
                            bar_hwnd: hwnd.0 as usize,
                            active: false,
                        },
                    );
                    bar.pressed = HitTarget::None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                if let Some(drag) = bar.drag_state.take() {
                    if drag.moved {
                        let mut rc = RECT::default();
                        let mut cursor_pt = POINT::default();
                        let anchor = GetCursorPos(&mut cursor_pt)
                            .ok()
                            .map(|_| (cursor_pt.x, cursor_pt.y));
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::DragEnd {
                                    bar_hwnd: hwnd.0 as usize,
                                    monitor_id: bar.monitor_id.clone(),
                                    screen_x: rc.left,
                                    screen_y: rc.top,
                                    dpi: bar.dpi,
                                    cursor_anchor: anchor,
                                },
                            );
                        }
                    }
                }
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                bar.hide_tooltip();
                if bar.variant_fan.is_active() {
                    let _ = KillTimer(Some(hwnd), TIMER_VARIANT_HOLD);
                    let mut cursor_pt = POINT::default();
                    let _ = GetCursorPos(&mut cursor_pt);
                    match bar.variant_fan.on_up(cursor_pt.x, cursor_pt.y) {
                        FanOutcome::EnteredSticky => {
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::FanActiveChanged {
                                    bar_hwnd: hwnd.0 as usize,
                                    active: true,
                                },
                            );
                            return LRESULT(0);
                        }
                        FanOutcome::ClickDefault { scene_id } => {
                            let _ = ReleaseCapture();
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::FanActiveChanged {
                                    bar_hwnd: hwnd.0 as usize,
                                    active: false,
                                },
                            );
                            let pressed = std::mem::replace(&mut bar.pressed, HitTarget::None);
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                            let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                            if bar.layout.hit_test(client_x, client_y) == pressed {
                                if let Some(ref mut ov) = bar.overflow_popup {
                                    ov.hide();
                                }
                                push_bar_event(
                                    bar.host_hwnd,
                                    BarEvent::SwitchScene {
                                        bar_hwnd: hwnd.0 as usize,
                                        monitor_id: bar.monitor_id.clone(),
                                        scene_id,
                                        variant_id: None,
                                        source: SwitchSource::BarClick,
                                    },
                                );
                            }
                            return LRESULT(0);
                        }
                        FanOutcome::CommitVariant {
                            scene_id,
                            variant_id,
                        } => {
                            let _ = ReleaseCapture();
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::FanActiveChanged {
                                    bar_hwnd: hwnd.0 as usize,
                                    active: false,
                                },
                            );
                            bar.pressed = HitTarget::None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            if let Some(ref mut ov) = bar.overflow_popup {
                                ov.hide();
                            }
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::SwitchScene {
                                    bar_hwnd: hwnd.0 as usize,
                                    monitor_id: bar.monitor_id.clone(),
                                    scene_id,
                                    variant_id: Some(variant_id),
                                    source: SwitchSource::BarClick,
                                },
                            );
                            return LRESULT(0);
                        }
                        FanOutcome::Cancelled => {
                            let _ = ReleaseCapture();
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::FanActiveChanged {
                                    bar_hwnd: hwnd.0 as usize,
                                    active: false,
                                },
                            );
                            bar.pressed = HitTarget::None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            return LRESULT(0);
                        }
                        FanOutcome::None | FanOutcome::Opened | FanOutcome::Updated => {
                            return LRESULT(0);
                        }
                    }
                }
                let pressed = std::mem::replace(&mut bar.pressed, HitTarget::None);
                if let Some(drag) = bar.drag_state.take() {
                    let _ = ReleaseCapture();
                    if drag.moved {
                        let mut rc = RECT::default();
                        let mut cursor_pt = POINT::default();
                        let anchor = GetCursorPos(&mut cursor_pt)
                            .ok()
                            .map(|_| (cursor_pt.x, cursor_pt.y));
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::DragEnd {
                                    bar_hwnd: hwnd.0 as usize,
                                    monitor_id: bar.monitor_id.clone(),
                                    screen_x: rc.left,
                                    screen_y: rc.top,
                                    dpi: bar.dpi,
                                    cursor_anchor: anchor,
                                },
                            );
                        }
                    }
                    return LRESULT(0);
                }
                if pressed != HitTarget::None {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }

                let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
                let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let hit = bar.layout.hit_test(client_x, client_y);
                if hit != pressed {
                    return LRESULT(0);
                }
                match hit {
                    HitTarget::SceneButton { scene_id, .. } => {
                        if let Some(ref mut ov) = bar.overflow_popup {
                            ov.hide();
                        }
                        push_bar_event(
                            bar.host_hwnd,
                            BarEvent::SwitchScene {
                                bar_hwnd: hwnd.0 as usize,
                                monitor_id: bar.monitor_id.clone(),
                                scene_id,
                                variant_id: None,
                                source: SwitchSource::BarClick,
                            },
                        );
                    }
                    HitTarget::OverflowButton => {
                        push_bar_event(
                            bar.host_hwnd,
                            BarEvent::OverflowOpening {
                                bar_hwnd: hwnd.0 as usize,
                            },
                        );
                        if let Some(ref ov_btn_rc) = bar.layout.overflow_button_rect {
                            let mut win_rc = RECT::default();
                            let _ = GetWindowRect(hwnd, &mut win_rc);
                            let anchor_screen = RectData {
                                left: win_rc.left + ov_btn_rc.left,
                                top: win_rc.top + ov_btn_rc.top,
                                right: win_rc.left + ov_btn_rc.right,
                                bottom: win_rc.top + ov_btn_rc.bottom,
                            };
                            let overflow_scenes = bar.snapshot.overflow_scenes.clone();
                            let active_id = bar.snapshot.active_scene_id.clone();
                            let work_area = bar.work_area;
                            let dpi = bar.dpi;
                            if let Some(ref mut ov) = bar.overflow_popup {
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
                if let Some(scene) = bar.snapshot.overflow_scenes.get(idx) {
                    let scene_id = scene.id.clone();
                    let variant_id = usize::try_from(lparam.0)
                        .ok()
                        .and_then(|v| v.checked_sub(1))
                        .and_then(|v_idx| scene.variants.get(v_idx))
                        .map(|v| v.id.clone());
                    push_bar_event(
                        bar.host_hwnd,
                        BarEvent::SwitchScene {
                            bar_hwnd: hwnd.0 as usize,
                            monitor_id: bar.monitor_id.clone(),
                            scene_id,
                            variant_id,
                            source: SwitchSource::Overflow,
                        },
                    );
                }
                LRESULT(0)
            }

            WM_WINDOWPOSCHANGED => {
                if !bar.applying_bounds && bar.drag_state.is_none() && lparam.0 != 0 {
                    let wp = &*(lparam.0 as *const WINDOWPOS);
                    if (wp.flags.0 & SWP_NOZORDER.0) == 0 {
                        bar.schedule_topmost_check();
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_DPICHANGED => {
                bar.cancel_bar_variant_fan();
                if !bar.applying_bounds {
                    push_bar_event(bar.host_hwnd, BarEvent::DisplayOrDpiChanged);
                }
                LRESULT(0)
            }

            WM_TIMER => {
                match wparam.0 {
                    TIMER_TOPMOST_CHECK => {
                        let _ = KillTimer(Some(hwnd), TIMER_TOPMOST_CHECK);
                        bar.ensure_topmost_if_needed(false);
                    }
                    TIMER_VARIANT_HOLD => {
                        let _ = KillTimer(Some(hwnd), TIMER_VARIANT_HOLD);
                        let mut cursor_pt = POINT::default();
                        if GetCursorPos(&mut cursor_pt).is_ok()
                            && matches!(
                                bar.variant_fan.on_hold_timer(cursor_pt.x, cursor_pt.y),
                                FanOutcome::Opened
                            )
                        {
                            push_bar_event(
                                bar.host_hwnd,
                                BarEvent::FanActiveChanged {
                                    bar_hwnd: hwnd.0 as usize,
                                    active: true,
                                },
                            );
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let paint_state = PaintState {
                    layout: &bar.layout,
                    scene_state: &bar.snapshot.scene_state,
                    last_known_scene_id: bar.snapshot.last_known_scene_id.as_deref(),
                    active_in_overflow: bar.snapshot.active_in_overflow,
                    hovered: &bar.hovered,
                    pressed: &bar.pressed,
                    has_warnings: bar.snapshot.warning_count > 0,
                };
                paint_launcher(hdc, &mut bar.theme, &paint_state);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            WM_DESTROY => LRESULT(0),

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    match result {
        Ok(lres) => lres,
        Err(_) => {
            log_error("wndproc", "Caught panic across FFI boundary in bar_wndproc");
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::platform::fake::FakeWindowSystem;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    #[test]
    fn drag_reuses_the_layout_until_crossing_to_a_different_dpi() {
        let mut config = AppConfig::default_initial();
        config.launcher.always_on_top = false;
        let (visible_scenes, overflow_scenes) = config.split_visible_and_overflow();
        let snapshot = Rc::new(BarSnapshot {
            launcher_cfg: config.launcher,
            visible_scenes,
            overflow_scenes,
            active_scene_id: None,
            scene_state: SceneState::Unknown,
            last_known_scene_id: None,
            active_in_overflow: false,
            warning_count: 0,
        });
        // Keep the native HWND offscreen while simulating two displays with different DPI.
        let mut monitors = FakeWindowSystem::new_dual_monitor().monitors;
        for mon in &mut monitors {
            mon.monitor_rect.top -= 12000;
            mon.monitor_rect.bottom -= 12000;
            mon.work_area.top -= 12000;
            mon.work_area.bottom -= 12000;
        }
        monitors[1].dpi = 144;
        let initial_layout = snapshot.compute_layout(96);
        let resolved = ResolvedLauncherBar {
            monitor_id: monitors[0].monitor_id.clone(),
            x: 100,
            y: -11900,
            width_px: initial_layout.width_px,
            height_px: initial_layout.height_px,
            dpi: 96,
        };
        let second_id = monitors[1].monitor_id.clone();
        let mut bar = LauncherBarWindow::create_and_show(
            HWND::default(),
            &resolved,
            monitors[0].work_area,
            snapshot,
        )
        .unwrap();
        bar.drag_state = Some(LauncherDragState {
            grab_rel_x: 0.5,
            grab_rel_y: 0.5,
            start_cursor_x: 100,
            start_cursor_y: -11900,
            moved: false,
            monitors,
        });
        assert!(!bar.layout.scene_buttons.is_empty());
        let buttons = bar.layout.scene_buttons.as_ptr();
        for x in 200..230 {
            bar.update_drag_position(x, -11900);
            assert_eq!(bar.layout.scene_buttons.as_ptr(), buttons);
        }
        let mut rect = RECT::default();
        unsafe { GetWindowRect(bar.hwnd, &mut rect).unwrap() };
        assert_eq!(
            rect.left,
            229 - (0.5 * resolved.width_px as f64).round() as i32
        );
        assert_eq!(rect.right - rect.left, resolved.width_px);

        bar.update_drag_position(2500, -11900);
        unsafe { GetWindowRect(bar.hwnd, &mut rect).unwrap() };
        assert_eq!(bar.monitor_id, second_id);
        assert_eq!(bar.layout.dpi, 144);
        assert_eq!(rect.right - rect.left, bar.layout.width_px);
        assert!(bar.layout.width_px > resolved.width_px);
        bar.update_drag_position(300, -11900);
        assert_eq!(bar.monitor_id, resolved.monitor_id);
        assert_eq!(bar.layout.width_px, resolved.width_px);
    }
}
