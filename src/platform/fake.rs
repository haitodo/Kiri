//! Fake `WindowSystem` implementation for deterministic unit and acceptance testing.
//!
//! Simulates:
//! - Multi-monitor setups (Monitor 1 + Monitor 2)
//! - Z-order changes when windows are restored or minimized
//! - Foreground window changes on `SW_MINIMIZE` vs `SW_SHOWMINNOACTIVE`
//! - Modal dialog disabled windows (`is_enabled == false`)
//! - Hung windows (`is_hung == true`)
//! - Elevated windows (`is_elevated == true`)
//! - Tray-hiding windows (windows that set `is_visible = false, is_iconic = false` when minimized)
//! - Maximized windows (`is_maximized == true`)
//! - Manual user minimize/restore simulation

use crate::config::RectData;
use crate::platform::{MonitorInfoData, PlatformError, ShowCmd, WindowInstance, WindowSystem};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct IssuedCommand {
    pub hwnd: u64,
    pub cmd: ShowCmd,
}

pub struct FakeWindowSystem {
    pub windows: Vec<WindowInstance>,
    pub monitors: Vec<MonitorInfoData>,
    pub foreground_hwnd: Option<u64>,
    pub last_input_tick: u32,
    pub issued_commands: Vec<IssuedCommand>,
    /// HWNDs that hide themselves to the system tray (`is_visible = false, is_iconic = false`) when minimized.
    pub tray_hiding_hwnds: HashSet<u64>,
    /// HWNDs that ignore `ShowWindowAsync` (simulate failure to transition state).
    pub ignore_commands_hwnds: HashSet<u64>,
}

impl Default for FakeWindowSystem {
    fn default() -> Self {
        Self::new_dual_monitor()
    }
}

impl FakeWindowSystem {
    pub fn new_dual_monitor() -> Self {
        let mon1 = MonitorInfoData {
            monitor_id: "MON-1-PRIMARY".to_string(),
            friendly_name: "Primary Monitor 1".to_string(),
            monitor_rect: RectData {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            work_area: RectData {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
            dpi: 96,
            is_primary: true,
        };
        let mon2 = MonitorInfoData {
            monitor_id: "MON-2-TARGET".to_string(),
            friendly_name: "Secondary Monitor 2".to_string(),
            monitor_rect: RectData {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            work_area: RectData {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1040,
            },
            dpi: 96,
            is_primary: false,
        };
        Self {
            windows: Vec::new(),
            monitors: vec![mon1, mon2],
            foreground_hwnd: None,
            last_input_tick: 0,
            issued_commands: Vec::new(),
            tray_hiding_hwnds: HashSet::new(),
            ignore_commands_hwnds: HashSet::new(),
        }
    }

    pub fn add_window(&mut self, mut win: WindowInstance) {
        win.z_order = self.windows.len();
        self.windows.push(win);
    }

    pub fn recompute_z_order(&mut self) {
        for (idx, w) in self.windows.iter_mut().enumerate() {
            w.z_order = idx;
        }
    }

    /// Simulate user manually restoring a window from the taskbar.
    pub fn simulate_user_restore(&mut self, hwnd: u64) {
        if let Some(idx) = self.windows.iter().position(|w| w.hwnd == hwnd) {
            self.windows[idx].is_iconic = false;
            self.windows[idx].is_visible = true;
            let win = self.windows.remove(idx);
            self.windows.insert(0, win);
            self.recompute_z_order();
            self.foreground_hwnd = Some(hwnd);
        }
    }

    /// Simulate user manually minimizing a window.
    pub fn simulate_user_minimize(&mut self, hwnd: u64) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.hwnd == hwnd) {
            w.is_iconic = true;
        }
    }
}

impl WindowSystem for FakeWindowSystem {
    fn enumerate_windows(&self) -> Vec<WindowInstance> {
        self.windows
            .iter()
            .filter(|w| w.is_visible)
            .cloned()
            .enumerate()
            .map(|(idx, mut w)| {
                w.z_order = idx;
                w
            })
            .collect()
    }

    fn query_window(&self, hwnd: u64) -> Option<WindowInstance> {
        self.windows.iter().find(|w| w.hwnd == hwnd).cloned()
    }

    fn show_window_async(&mut self, hwnd: u64, cmd: ShowCmd) -> Result<(), PlatformError> {
        let Some(idx) = self.windows.iter().position(|w| w.hwnd == hwnd) else {
            return Err(PlatformError::WindowNotFound(hwnd));
        };

        if self.windows[idx].is_elevated {
            return Err(PlatformError::AccessDenied(hwnd));
        }

        self.issued_commands.push(IssuedCommand { hwnd, cmd });

        if self.windows[idx].is_hung || self.ignore_commands_hwnds.contains(&hwnd) {
            // Async request was sent, but window does not actually change state
            return Ok(());
        }

        match cmd {
            ShowCmd::MinimizeNoActivate => {
                if self.tray_hiding_hwnds.contains(&hwnd) {
                    self.windows[idx].is_iconic = false;
                    self.windows[idx].is_visible = false;
                } else {
                    self.windows[idx].is_iconic = true;
                }
                // Note: SW_SHOWMINNOACTIVE intentionally does NOT change foreground_hwnd!
            }
            ShowCmd::MinimizeActivateNext => {
                if self.tray_hiding_hwnds.contains(&hwnd) {
                    self.windows[idx].is_iconic = false;
                    self.windows[idx].is_visible = false;
                } else {
                    self.windows[idx].is_iconic = true;
                }
                // SW_MINIMIZE activates the next visible non-iconic window in Z-order
                if self.foreground_hwnd == Some(hwnd) {
                    self.foreground_hwnd = self
                        .windows
                        .iter()
                        .find(|w| w.hwnd != hwnd && w.is_visible && !w.is_iconic)
                        .map(|w| w.hwnd);
                }
            }
            ShowCmd::RestoreNoActivate => {
                self.windows[idx].is_iconic = false;
                self.windows[idx].is_visible = true;
                // Move to top of non-active Z-order ( preserving relative order of subsequent restores)
                let win = self.windows.remove(idx);
                self.windows.insert(0, win);
                self.recompute_z_order();
            }
            ShowCmd::RestoreActivate => {
                // Per Win32 behavior: sending SW_RESTORE to a non-iconic maximized window un-maximizes it!
                if !self.windows[idx].is_iconic && self.windows[idx].is_maximized {
                    self.windows[idx].is_maximized = false;
                }
                self.windows[idx].is_iconic = false;
                self.windows[idx].is_visible = true;
                let win = self.windows.remove(idx);
                self.windows.insert(0, win);
                self.recompute_z_order();
                self.foreground_hwnd = Some(hwnd);
            }
        }

        Ok(())
    }

    fn bring_to_top(&mut self, hwnd: u64) -> bool {
        if let Some(pos) = self
            .windows
            .iter()
            .position(|w| w.hwnd == hwnd && w.is_visible && !w.is_iconic && !w.is_hung)
        {
            let win = self.windows.remove(pos);
            self.windows.insert(0, win);
            self.recompute_z_order();
            true
        } else {
            false
        }
    }

    fn get_foreground_window(&self) -> Option<u64> {
        self.foreground_hwnd
    }

    fn set_foreground_window(&mut self, hwnd: u64) -> bool {
        if self.windows.iter().any(|w| w.hwnd == hwnd && w.is_enabled) && self.bring_to_top(hwnd) {
            self.foreground_hwnd = Some(hwnd);
            return true;
        }
        false
    }

    fn last_input_tick(&self) -> Option<u32> {
        Some(self.last_input_tick)
    }

    fn set_window_placement(
        &mut self,
        hwnd: u64,
        normal_rect: RectData,
        maximized: bool,
    ) -> Result<(), PlatformError> {
        let Some(w) = self.windows.iter_mut().find(|w| w.hwnd == hwnd) else {
            return Err(PlatformError::WindowNotFound(hwnd));
        };
        if w.is_elevated {
            return Err(PlatformError::AccessDenied(hwnd));
        }
        w.normal_rect = normal_rect;
        w.window_rect = normal_rect;
        w.is_maximized = maximized;
        Ok(())
    }

    fn monitors(&self) -> Vec<MonitorInfoData> {
        self.monitors.clone()
    }
}
