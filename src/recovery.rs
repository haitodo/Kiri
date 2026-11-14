//! Write-ahead runtime journal (`runtime-state.json`), `ManagedAction` tracking,
//! WinEvent user-override invalidation, crash recovery, and "Restore All" fail-safe.
//!
//! Per Section 4.1, 5.3, 6, 8.1, and 8.2 of spec.md:
//! - Before sending `ShowWindowAsync` to minimize any window, `ManagedAction` is written
//!   to `runtime-state.json` with `state = Pending` (Write-Ahead Logging).
//! - Once the minimize request is issued, `state` transitions to `MinimizedByUs`.
//! - If a user manually restores a window (`EVENT_SYSTEM_MINIMIZEEND` while not being restored by us),
//!   its `ManagedAction` is marked `Invalidated` so subsequent manual re-minimization is never
//!   mistaken for a Kiri minimization (FR-08, AT-23).
//! - On normal exit, all `MinimizedByUs` windows are restored and `runtime-state.json` is cleared.
//! - On logoff/shutdown (`WM_ENDSESSION`), windows are NOT restored and the journal is flushed as-is.
//! - On startup after a crash, surviving windows matching `(hwnd, pid, process_start_time)` that are
//!   still `is_iconic` and in `Pending` or `MinimizedByUs` state are identified and can be restored.

use crate::config::{atomic_replace_file, RectData};
use crate::diagnostics::{current_unix_ms, log_error, log_info, log_warn};
use crate::platform::hooks::{MinimizeEvent, MinimizeEventKind};
use crate::platform::{ShowCmd, WindowInstance, WindowSystem};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedActionState {
    /// Written to journal BEFORE `ShowWindowAsync` is called.
    Pending,
    /// `ShowWindowAsync` was called by Kiri to minimize this window.
    MinimizedByUs,
    /// User manually restored (or manually interacted with) the window; do not auto-restore.
    Invalidated,
    /// App hid itself from taskbar/screen upon minimize (`!is_visible && !is_iconic`, Section 8.2).
    HiddenToTray,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAction {
    pub binding_id: String,
    pub hwnd: u64,
    pub pid: u32,
    pub process_start_time: u64,
    pub monitor_id: String,
    pub normal_rect: RectData,
    pub was_maximized: bool,
    pub z_order_at_minimize: usize,
    pub has_taskbar_button: bool,
    pub state: ManagedActionState,
    pub updated_at_ms: u64,
}

impl ManagedAction {
    /// Returns true if this action is eligible for automatic scene restoration.
    pub fn is_restorable_by_scene_switch(&self) -> bool {
        matches!(
            self.state,
            ManagedActionState::MinimizedByUs | ManagedActionState::Pending
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeJournalData {
    pub pid: u32,
    pub updated_at_ms: u64,
    pub actions: Vec<ManagedAction>,
}

pub struct RecoveryManager {
    journal_path: PathBuf,
    /// Active managed actions keyed by `hwnd`.
    actions: HashMap<u64, ManagedAction>,
    /// HWNDs for which Kiri itself recently issued a restore command
    /// (so the resulting `EVENT_SYSTEM_MINIMIZEEND` is recognized as ours, not a manual user restore).
    restoring_by_us: HashSet<u64>,
    /// Cached surviving actions loaded from a previous crashed session at startup.
    startup_crash_candidates: Vec<ManagedAction>,
}

impl RecoveryManager {
    pub fn new(journal_path: PathBuf) -> Self {
        Self {
            journal_path,
            actions: HashMap::new(),
            restoring_by_us: HashSet::new(),
            startup_crash_candidates: Vec::new(),
        }
    }

    pub fn default_journal_path() -> PathBuf {
        crate::diagnostics::default_app_data_dir().join("runtime-state.json")
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    pub fn actions(&self) -> &HashMap<u64, ManagedAction> {
        &self.actions
    }

    pub fn get_action(&self, hwnd: u64) -> Option<&ManagedAction> {
        self.actions.get(&hwnd)
    }

    pub fn startup_crash_candidates(&self) -> &[ManagedAction] {
        &self.startup_crash_candidates
    }

    /// Inspect `runtime-state.json` from a previous abnormal termination (Section 8.1 & AT-27).
    /// Identifies surviving windows whose `(hwnd, pid, process_start_time)` still match
    /// and are currently `is_iconic`.
    pub fn check_previous_crash_journal(&mut self, sys: &dyn WindowSystem) -> Vec<ManagedAction> {
        self.startup_crash_candidates.clear();
        if !self.journal_path.exists() {
            return Vec::new();
        }

        let Ok(mut file) = File::open(&self.journal_path) else {
            return Vec::new();
        };
        let mut raw = String::new();
        if file.read_to_string(&mut raw).is_err() {
            return Vec::new();
        }

        let Ok(prev_journal) = serde_json::from_str::<RuntimeJournalData>(&raw) else {
            log_warn("recovery", "Previous runtime-state.json was unreadable; ignoring");
            return Vec::new();
        };

        let mut candidates = Vec::new();
        for mut action in prev_journal.actions {
            if !matches!(
                action.state,
                ManagedActionState::Pending | ManagedActionState::MinimizedByUs
            ) {
                continue;
            }
            if let Some(live) = sys.query_window(action.hwnd) {
                if live.matches_runtime_identity(
                    action.hwnd,
                    action.pid,
                    action.process_start_time,
                ) && live.is_iconic
                {
                    action.state = ManagedActionState::MinimizedByUs;
                    self.actions.insert(action.hwnd, action.clone());
                    candidates.push(action);
                }
            }
        }

        if !candidates.is_empty() {
            log_warn(
                "recovery",
                &format!(
                    "Detected {} window(s) minimized before previous abnormal exit",
                    candidates.len()
                ),
            );
            let _ = self.persist_journal();
        } else {
            // No valid surviving minimized windows; clean up stale journal
            let _ = self.clear_journal();
        }

        self.startup_crash_candidates = candidates.clone();
        candidates
    }

    /// Step 6 of `switch_scene`: Record `to_minimize` windows as `Pending` and flush to `runtime-state.json`
    /// BEFORE issuing `ShowWindowAsync` commands.
    pub fn record_pending_minimizes(&mut self, items: &[(String, WindowInstance)]) {
        let now = current_unix_ms();
        for (binding_id, win) in items {
            self.restoring_by_us.remove(&win.hwnd);
            let action = ManagedAction {
                binding_id: binding_id.clone(),
                hwnd: win.hwnd,
                pid: win.pid,
                process_start_time: win.process_start_time,
                monitor_id: win.monitor_id.clone(),
                normal_rect: win.normal_rect,
                was_maximized: win.is_maximized,
                z_order_at_minimize: win.z_order,
                has_taskbar_button: win.has_taskbar_button,
                state: ManagedActionState::Pending,
                updated_at_ms: now,
            };
            self.actions.insert(win.hwnd, action);
        }
        if let Err(e) = self.persist_journal() {
            log_error("recovery", &format!("Failed to write pending journal: {}", e));
        }
    }

    /// Transition a `Pending` action to `MinimizedByUs` after `ShowWindowAsync` succeeds.
    pub fn mark_minimize_issued(&mut self, hwnd: u64) {
        if let Some(action) = self.actions.get_mut(&hwnd) {
            action.state = ManagedActionState::MinimizedByUs;
            action.updated_at_ms = current_unix_ms();
        }
    }

    /// Record a restore requested by Kiri so the resulting `MINIMIZEEND` event
    /// is not treated as an external manual override, and remove it from active minimized set.
    pub fn mark_restoring_by_us(&mut self, hwnd: u64) {
        self.restoring_by_us.insert(hwnd);
        self.actions.remove(&hwnd);
    }

    /// Mark an action as `Invalidated` (e.g. user manually restored the window or PID changed).
    pub fn invalidate_hwnd(&mut self, hwnd: u64) {
        if let Some(action) = self.actions.get_mut(&hwnd) {
            if action.state != ManagedActionState::Invalidated {
                action.state = ManagedActionState::Invalidated;
                action.updated_at_ms = current_unix_ms();
            }
        }
    }

    /// Mark an action as `HiddenToTray` when post-minimize verification finds `!is_visible && !is_iconic` (Section 8.2).
    pub fn mark_hidden_to_tray(&mut self, hwnd: u64) {
        if let Some(action) = self.actions.get_mut(&hwnd) {
            action.state = ManagedActionState::HiddenToTray;
            action.updated_at_ms = current_unix_ms();
        }
    }

    /// Process queued `MinimizeEvent`s from `SetWinEventHook` (Section 5.3 & AT-23).
    pub fn process_minimize_events(&mut self, events: &[MinimizeEvent]) {
        let mut dirty = false;
        for ev in events {
            match ev.kind {
                MinimizeEventKind::MinimizeEnd => {
                    // If Kiri itself initiated this restore, consume the marker and ignore
                    if self.restoring_by_us.remove(&ev.hwnd) {
                        continue;
                    }
                    // Otherwise, the user (or app) manually restored this window!
                    // Invalidate our ManagedAction so even if the user manually re-minimizes it later,
                    // Kiri will NOT auto-restore it on scene switch (AT-23).
                    if let Some(action) = self.actions.get_mut(&ev.hwnd) {
                        if matches!(
                            action.state,
                            ManagedActionState::MinimizedByUs | ManagedActionState::Pending
                        ) {
                            log_info(
                                "recovery",
                                &format!(
                                    "Window 0x{:X} (binding '{}') was restored externally; invalidating ManagedAction",
                                    ev.hwnd, action.binding_id
                                ),
                            );
                            action.state = ManagedActionState::Invalidated;
                            action.updated_at_ms = ev.timestamp_ms;
                            dirty = true;
                        }
                    }
                }
                MinimizeEventKind::MinimizeStart => {
                    // If the window is NOT in `Pending` or `MinimizedByUs`, this was a user-initiated minimize.
                    // We intentionally do NOT add it to `actions` (FR-08: only restore what Kiri minimized).
                }
            }
        }
        if dirty {
            let _ = self.persist_journal();
        }
    }

    /// Restore all windows currently minimized by Kiri (Section 8.1 "Kiriが最小化したウィンドウをすべて復元", AT-15).
    /// Restores in reverse Z-order (bottom to top) and returns the number of windows restored.
    pub fn restore_all_minimized_by_us(&mut self, sys: &mut dyn WindowSystem) -> usize {
        self.restore_minimized_by_us(sys, None)
    }

    pub fn restore_minimized_by_us(&mut self, sys: &mut dyn WindowSystem, windows: Option<&[WindowInstance]>) -> usize {
        let mut candidates: Vec<ManagedAction> = self
            .actions
            .values()
            .filter(|a| a.is_restorable_by_us_command() && windows.is_none_or(|windows| windows.iter().any(|win| {
                win.matches_runtime_identity(a.hwnd, a.pid, a.process_start_time)
            })))
            .cloned()
            .collect();

        // Sort by z_order descending (bottom-most window restored first so top-most ends up on top)
        candidates.sort_by(|a, b| b.z_order_at_minimize.cmp(&a.z_order_at_minimize));

        let mut restored_count = 0;
        for action in candidates {
            if let Some(live) = sys.query_window(action.hwnd) {
                if live.matches_runtime_identity(
                    action.hwnd,
                    action.pid,
                    action.process_start_time,
                ) && live.is_iconic
                {
                    if sys
                        .show_window_async(action.hwnd, ShowCmd::RestoreNoActivate)
                        .is_ok()
                    {
                        self.mark_restoring_by_us(action.hwnd);
                        restored_count += 1;
                    }
                } else {
                    self.actions.remove(&action.hwnd);
                }
            } else {
                self.actions.remove(&action.hwnd);
            }
        }

        let _ = self.persist_journal();
        log_info(
            "recovery",
            &format!("Restored {} window(s) via restore_all_minimized_by_us", restored_count),
        );
        restored_count
    }

    pub fn restorable_windows(&self, sys: &dyn WindowSystem) -> Vec<WindowInstance> {
        self.actions.values().filter(|action| action.is_restorable_by_us_command())
            .filter_map(|action| sys.query_window(action.hwnd).filter(|live| {
                live.is_iconic && live.matches_runtime_identity(
                    action.hwnd, action.pid, action.process_start_time
                )
            })).collect()
    }

    /// Flush current `actions` to `runtime-state.json` atomically.
    pub fn persist_journal(&self) -> Result<(), String> {
        if let Some(parent) = self.journal_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create journal dir: {}", e))?;
        }

        let data = RuntimeJournalData {
            pid: std::process::id(),
            updated_at_ms: current_unix_ms(),
            actions: self.actions.values().cloned().collect(),
        };

        let bytes = serde_json::to_vec_pretty(&data)
            .map_err(|e| format!("Failed to serialize runtime journal: {}", e))?;

        let tmp_path = self.journal_path.with_extension("json.tmp");
        {
            let mut file = File::create(&tmp_path)
                .map_err(|e| format!("Failed to create runtime journal tmp: {}", e))?;
            file.write_all(&bytes)
                .map_err(|e| format!("Failed to write runtime journal tmp: {}", e))?;
            file.sync_all()
                .map_err(|e| format!("Failed to flush runtime journal tmp: {}", e))?;
        }

        atomic_replace_file(&tmp_path, &self.journal_path, None)
    }

    /// Clear `runtime-state.json` on normal exit after restoring windows (Section 6).
    pub fn clear_journal(&mut self) -> Result<(), String> {
        self.actions.clear();
        self.restoring_by_us.clear();
        if self.journal_path.exists() {
            fs::remove_file(&self.journal_path)
                .map_err(|e| format!("Failed to remove runtime-state.json: {}", e))?;
        }
        Ok(())
    }
}

impl ManagedAction {
    fn is_restorable_by_us_command(&self) -> bool {
        matches!(
            self.state,
            ManagedActionState::MinimizedByUs | ManagedActionState::Pending
        )
    }
}
