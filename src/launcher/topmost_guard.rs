//! Low-overhead, conflict-safe Topmost maintenance guard for the always-on-top launcher.
//!
//! Design principles:
//! 1. Event-driven only (no background polling timers).
//! 2. Inspects only windows *above* the launcher via `GetWindow(GW_HWNDPREV)` (never `EnumWindows`).
//! 3. Intervenes only when `WS_EX_TOPMOST` is lost or an intersecting foreign window is above it.
//! 4. Respects fullscreen apps and transient Windows system UI (Start Menu, Alt+Tab, etc.).
//! 5. Breaks Z-order ping-pong loops with other topmost apps via cooldown + consecutive-conflict
//!    circuit breaker.

use crate::config::RectData;

pub const TOPMOST_DEBOUNCE_MS: u64 = 200;
pub const TOPMOST_COOLDOWN_MS: u64 = 150;
pub const TOPMOST_CONFLICT_WINDOW_MS: u64 = 3_000;
pub const TOPMOST_MAX_CONSECUTIVE_CONFLICTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopmostStatus {
    /// Launcher still has `WS_EX_TOPMOST` and no intersecting foreign window is above it.
    NotNeeded,
    /// A fullscreen app or presentation mode is active; do not fight it.
    SuppressedFullscreen,
    /// Transient Windows shell/system UI is currently above the launcher.
    SuppressedSystemUi,
    /// Launcher lost `WS_EX_TOPMOST` extended style.
    LostTopmostStyle,
    /// A foreign visible window intersects the launcher and sits above it in Z-order.
    Obstructed {
        blocker_hwnd: u64,
        blocker_class: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    NoOp,
    RaiseToTopmost,
    PausedEntered {
        blocker_hwnd: u64,
        blocker_class: String,
        conflicts: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboveWindowKind {
    IgnoreShell,
    TemporarySystemUi,
    AppWindow,
}

/// Classify a window sitting above the launcher by its Win32 class name.
pub fn classify_above_window(class_name: &str) -> AboveWindowKind {
    match class_name {
        "Progman"
        | "WorkerW"
        | "Shell_TrayWnd"
        | "Shell_SecondaryTrayWnd"
        | "tooltips_class32"
        | "IME"
        | "MSCTFIME UI" => AboveWindowKind::IgnoreShell,
        "TaskListThumbnailWnd"
        | "MultitaskingViewFrame"
        | "ForegroundStaging"
        | "NotifyIconOverflowWindow"
        | "TopLevelWindowForOverflowXamlIsland"
        | "XamlExplorerHostIslandWindow"
        | "Windows.UI.Core.CoreWindow" => AboveWindowKind::TemporarySystemUi,
        _ => AboveWindowKind::AppWindow,
    }
}

/// Returns true if a foreign foreground window covers the entire monitor rect where it lives
/// and is not a desktop shell window.
pub fn is_borderless_fullscreen_rect(
    fg_class: &str,
    fg_rect: RectData,
    monitor_rect: RectData,
    launcher_rect: RectData,
) -> bool {
    if matches!(
        fg_class,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return false;
    }
    let covers_monitor = fg_rect.left <= monitor_rect.left
        && fg_rect.top <= monitor_rect.top
        && fg_rect.right >= monitor_rect.right
        && fg_rect.bottom >= monitor_rect.bottom
        && monitor_rect.width() > 0
        && monitor_rect.height() > 0;
    covers_monitor && fg_rect.intersection_area(&launcher_rect) > 0
}

#[derive(Debug, Default)]
pub struct TopmostGuard {
    paused: bool,
    last_raise_ms: Option<u64>,
    consecutive_conflicts: u32,
}

impl TopmostGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Clear conflict pause when the user explicitly interacts with Kiri or display layout resets.
    pub fn on_user_interaction(&mut self) {
        self.paused = false;
        self.consecutive_conflicts = 0;
    }

    /// Evaluate the inspected Z-order status and decide whether to re-apply `HWND_TOPMOST`.
    pub fn evaluate(&mut self, now_ms: u64, status: TopmostStatus) -> GuardDecision {
        match status {
            TopmostStatus::NotNeeded => {
                if let Some(last) = self.last_raise_ms {
                    if now_ms.saturating_sub(last) > TOPMOST_CONFLICT_WINDOW_MS {
                        self.consecutive_conflicts = 0;
                    }
                }
                GuardDecision::NoOp
            }
            TopmostStatus::SuppressedFullscreen | TopmostStatus::SuppressedSystemUi => {
                // Legitimate temporary overlay or fullscreen app; do not penalize conflict count.
                GuardDecision::NoOp
            }
            TopmostStatus::LostTopmostStyle => {
                self.handle_obstruction(now_ms, 0, String::from("LostTopmostStyle"))
            }
            TopmostStatus::Obstructed {
                blocker_hwnd,
                blocker_class,
            } => self.handle_obstruction(now_ms, blocker_hwnd, blocker_class),
        }
    }

    fn handle_obstruction(
        &mut self,
        now_ms: u64,
        blocker_hwnd: u64,
        blocker_class: String,
    ) -> GuardDecision {
        if self.paused {
            return GuardDecision::NoOp;
        }

        if let Some(last) = self.last_raise_ms {
            let elapsed = now_ms.saturating_sub(last);
            if elapsed < TOPMOST_COOLDOWN_MS {
                return GuardDecision::NoOp;
            }
            if elapsed <= TOPMOST_CONFLICT_WINDOW_MS {
                self.consecutive_conflicts += 1;
            } else {
                self.consecutive_conflicts = 0;
            }
        }

        if self.consecutive_conflicts >= TOPMOST_MAX_CONSECUTIVE_CONFLICTS {
            self.paused = true;
            return GuardDecision::PausedEntered {
                blocker_hwnd,
                blocker_class,
                conflicts: self.consecutive_conflicts,
            };
        }

        self.last_raise_ms = Some(now_ms);
        GuardDecision::RaiseToTopmost
    }
}

#[cfg(windows)]
pub fn inspect_topmost_status(
    launcher_hwnd: windows::Win32::Foundation::HWND,
    own_pid: u32,
) -> TopmostStatus {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, GWL_EXSTYLE, GW_HWNDPREV, WS_EX_TOPMOST,
    };

    const MAX_ABOVE_WALK: usize = 64;

    unsafe {
        if !IsWindowVisible(launcher_hwnd).as_bool() || IsIconic(launcher_hwnd).as_bool() {
            return TopmostStatus::NotNeeded;
        }

        let mut raw_rc = RECT::default();
        if GetWindowRect(launcher_hwnd, &mut raw_rc).is_err() {
            return TopmostStatus::NotNeeded;
        }
        let launcher_rc = RectData {
            left: raw_rc.left,
            top: raw_rc.top,
            right: raw_rc.right,
            bottom: raw_rc.bottom,
        };
        if launcher_rc.width() <= 0 || launcher_rc.height() <= 0 {
            return TopmostStatus::NotNeeded;
        }

        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN
                || state == QUNS_PRESENTATION_MODE
                || state == QUNS_BUSY
            {
                return TopmostStatus::SuppressedFullscreen;
            }
        }

        let fg = GetForegroundWindow();
        if !fg.0.is_null() && fg != launcher_hwnd {
            let mut fg_pid: u32 = 0;
            GetWindowThreadProcessId(fg, Some(&mut fg_pid));
            if fg_pid != 0 && fg_pid != own_pid && IsWindowVisible(fg).as_bool() && !IsIconic(fg).as_bool() {
                let mut fg_raw_rc = RECT::default();
                if GetWindowRect(fg, &mut fg_raw_rc).is_ok() {
                    let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
                    let mut mi = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    if !hmon.is_invalid() && GetMonitorInfoW(hmon, &mut mi).as_bool() {
                        let fg_class = read_class_name(fg);
                        let fg_rc = RectData {
                            left: fg_raw_rc.left,
                            top: fg_raw_rc.top,
                            right: fg_raw_rc.right,
                            bottom: fg_raw_rc.bottom,
                        };
                        let mon_rc = RectData {
                            left: mi.rcMonitor.left,
                            top: mi.rcMonitor.top,
                            right: mi.rcMonitor.right,
                            bottom: mi.rcMonitor.bottom,
                        };
                        if is_borderless_fullscreen_rect(&fg_class, fg_rc, mon_rc, launcher_rc) {
                            return TopmostStatus::SuppressedFullscreen;
                        }
                    }
                }
            }
        }

        let has_topmost = (GetWindowLongW(launcher_hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0;

        let mut cur: Option<HWND> = GetWindow(launcher_hwnd, GW_HWNDPREV).ok();
        let mut steps = 0usize;
        while let Some(other) = cur {
            if other.0.is_null() || steps >= MAX_ABOVE_WALK {
                break;
            }
            steps += 1;
            let next = GetWindow(other, GW_HWNDPREV).ok();

            if !IsWindowVisible(other).as_bool() || IsIconic(other).as_bool() {
                cur = next;
                continue;
            }

            let mut other_pid: u32 = 0;
            GetWindowThreadProcessId(other, Some(&mut other_pid));
            if other_pid == own_pid || other_pid == 0 {
                cur = next;
                continue;
            }

            let mut other_raw_rc = RECT::default();
            if GetWindowRect(other, &mut other_raw_rc).is_err() {
                cur = next;
                continue;
            }
            let other_rc = RectData {
                left: other_raw_rc.left,
                top: other_raw_rc.top,
                right: other_raw_rc.right,
                bottom: other_raw_rc.bottom,
            };
            if other_rc.intersection_area(&launcher_rc) == 0 {
                cur = next;
                continue;
            }

            let mut cloaked: u32 = 0;
            if DwmGetWindowAttribute(
                other,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok()
                && cloaked != 0
            {
                cur = next;
                continue;
            }

            let class_name = read_class_name(other);
            match classify_above_window(&class_name) {
                AboveWindowKind::IgnoreShell => {
                    cur = next;
                    continue;
                }
                AboveWindowKind::TemporarySystemUi => {
                    return TopmostStatus::SuppressedSystemUi;
                }
                AboveWindowKind::AppWindow => {
                    return TopmostStatus::Obstructed {
                        blocker_hwnd: other.0 as usize as u64,
                        blocker_class: class_name,
                    };
                }
            }
        }

        if !has_topmost {
            TopmostStatus::LostTopmostStyle
        } else {
            TopmostStatus::NotNeeded
        }
    }
}

#[cfg(windows)]
unsafe fn read_class_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 128];
    let len = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(windows)]
pub fn raise_launcher_to_topmost(launcher_hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    };

    unsafe {
        SetWindowPos(
            launcher_hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obstructed(hwnd: u64) -> TopmostStatus {
        TopmostStatus::Obstructed {
            blocker_hwnd: hwnd,
            blocker_class: String::from("OtherTopmostApp"),
        }
    }

    #[test]
    fn raises_once_when_obstructed_or_style_lost() {
        let mut guard = TopmostGuard::new();
        assert_eq!(guard.evaluate(1000, TopmostStatus::NotNeeded), GuardDecision::NoOp);
        assert_eq!(guard.evaluate(1100, obstructed(10)), GuardDecision::RaiseToTopmost);
        assert!(!guard.is_paused());

        // Style loss after conflict window works cleanly as well
        assert_eq!(
            guard.evaluate(5000, TopmostStatus::LostTopmostStyle),
            GuardDecision::RaiseToTopmost
        );
    }

    #[test]
    fn cooldown_ignores_immediate_bounce_after_raise() {
        let mut guard = TopmostGuard::new();
        assert_eq!(guard.evaluate(1000, obstructed(10)), GuardDecision::RaiseToTopmost);
        assert_eq!(
            guard.evaluate(1000 + TOPMOST_COOLDOWN_MS - 1, obstructed(10)),
            GuardDecision::NoOp
        );
        // Once cooldown passes, re-raise is permitted
        assert_eq!(
            guard.evaluate(1000 + TOPMOST_COOLDOWN_MS, obstructed(10)),
            GuardDecision::RaiseToTopmost
        );
    }

    #[test]
    fn circuit_breaker_pauses_after_consecutive_conflicts_and_resets_on_user_action() {
        let mut guard = TopmostGuard::new();
        let mut t = 1_000;
        // Initial raise (conflict count = 0)
        assert_eq!(guard.evaluate(t, obstructed(99)), GuardDecision::RaiseToTopmost);

        // Consecutive bounces within CONFLICT_WINDOW_MS increment conflict counter
        for expected_conflict in 1..TOPMOST_MAX_CONSECUTIVE_CONFLICTS {
            t += 400;
            assert_eq!(
                guard.evaluate(t, obstructed(99)),
                GuardDecision::RaiseToTopmost,
                "conflict {expected_conflict} should still retry"
            );
        }

        // Next bounce reaches TOPMOST_MAX_CONSECUTIVE_CONFLICTS -> pause
        t += 400;
        assert_eq!(
            guard.evaluate(t, obstructed(99)),
            GuardDecision::PausedEntered {
                blocker_hwnd: 99,
                blocker_class: String::from("OtherTopmostApp"),
                conflicts: TOPMOST_MAX_CONSECUTIVE_CONFLICTS,
            }
        );
        assert!(guard.is_paused());

        // Subsequent obstructions stay paused without fighting
        t += 10_000;
        assert_eq!(guard.evaluate(t, obstructed(99)), GuardDecision::NoOp);

        // User interaction resets pause
        guard.on_user_interaction();
        assert!(!guard.is_paused());
        assert_eq!(guard.evaluate(t + 100, obstructed(99)), GuardDecision::RaiseToTopmost);
    }

    #[test]
    fn fullscreen_and_system_ui_suppression_do_not_consume_conflict_budget() {
        let mut guard = TopmostGuard::new();
        assert_eq!(guard.evaluate(1000, obstructed(10)), GuardDecision::RaiseToTopmost);

        assert_eq!(
            guard.evaluate(1400, TopmostStatus::SuppressedFullscreen),
            GuardDecision::NoOp
        );
        assert_eq!(
            guard.evaluate(1800, TopmostStatus::SuppressedSystemUi),
            GuardDecision::NoOp
        );
        assert!(!guard.is_paused());

        // Conflict count should still be 0 -> 1 on next actual conflict, not paused
        assert_eq!(guard.evaluate(2200, obstructed(10)), GuardDecision::RaiseToTopmost);
        assert!(!guard.is_paused());
    }

    #[test]
    fn conflict_counter_decays_after_quiet_period() {
        let mut guard = TopmostGuard::new();
        assert_eq!(guard.evaluate(1000, obstructed(10)), GuardDecision::RaiseToTopmost);
        assert_eq!(guard.evaluate(1500, obstructed(10)), GuardDecision::RaiseToTopmost);
        assert_eq!(guard.evaluate(2000, obstructed(10)), GuardDecision::RaiseToTopmost);
        // Now consecutive_conflicts == 2; wait beyond CONFLICT_WINDOW_MS
        assert_eq!(
            guard.evaluate(2000 + TOPMOST_CONFLICT_WINDOW_MS + 1, TopmostStatus::NotNeeded),
            GuardDecision::NoOp
        );
        // Should be reset and allow normal raise again
        assert_eq!(
            guard.evaluate(2000 + TOPMOST_CONFLICT_WINDOW_MS + 200, obstructed(10)),
            GuardDecision::RaiseToTopmost
        );
        assert!(!guard.is_paused());
    }

    #[test]
    fn borderless_fullscreen_detection_ignores_desktop_and_non_intersecting_monitors() {
        let mon = RectData {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let launcher_on_mon1 = RectData {
            left: 100,
            top: 20,
            right: 260,
            bottom: 60,
        };
        let launcher_on_mon2 = RectData {
            left: 2000,
            top: 20,
            right: 2160,
            bottom: 60,
        };

        assert!(!is_borderless_fullscreen_rect(
            "Progman",
            mon,
            mon,
            launcher_on_mon1
        ));
        assert!(is_borderless_fullscreen_rect(
            "Chrome_WidgetWin_1",
            mon,
            mon,
            launcher_on_mon1
        ));
        assert!(!is_borderless_fullscreen_rect(
            "Chrome_WidgetWin_1",
            mon,
            mon,
            launcher_on_mon2
        ));
    }

    #[test]
    fn classifies_shell_and_transient_system_ui_classes() {
        assert_eq!(classify_above_window("Shell_TrayWnd"), AboveWindowKind::IgnoreShell);
        assert_eq!(
            classify_above_window("XamlExplorerHostIslandWindow"),
            AboveWindowKind::TemporarySystemUi
        );
        assert_eq!(
            classify_above_window("ApplicationFrameWindow"),
            AboveWindowKind::AppWindow
        );
    }
}
