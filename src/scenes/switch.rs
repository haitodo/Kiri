//! Core scene-switching differential engine and post-switch verification logic.
//!
//! Implements the 10-step algorithm in Section 5.1, plus Sections 5.3, 5.4, 8.2:
//! - Non-destructive, idempotent, differential window state transitions.
//! - Protects windows on non-target monitors (FR-05, AT-06, AT-22).
//! - Leaves unregistered windows completely untouched (AT-08).
//! - Keeps windows shared between scenes or marked `always_visible` untouched (FR-07, AT-07).
//! - Restores ONLY windows minimized by Kiri (`ManagedActionState::MinimizedByUs`, FR-08, AT-09, AT-23).
//! - Skips disabled windows (`!is_enabled`, e.g. modal order confirmation dialog open, FR-17, AT-26) and warns.
//! - Prevents keyboard input stranding on minimized foreground windows by minimizing the foreground window
//!   LAST using `SW_MINIMIZE` after restoring target scene windows (FR-18, Section 5.4, AT-25).
//! - Restores windows in reverse Z-order (bottom-to-top) after verifying `is_iconic` (Section 5.3).
//! - Verifies window states on a one-shot timer (Step 10) and detects tray-hiding apps (Section 8.2).

use crate::config::AppConfig;
use crate::diagnostics::{log_info, log_warn, mask_title};
use crate::platform::{ShowCmd, WindowInstance, WindowSystem};
use crate::recovery::RecoveryManager;
use crate::scenes::bindings::{BindingResolution, BindingResolver};
use crate::scenes::manager::SceneStateManager;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchWarning {
    /// Target monitor is not configured (`target.monitor_id` is None).
    TargetMonitorNotConfigured,
    /// Target monitor is currently disconnected.
    TargetMonitorDisconnected { monitor_id: String },
    /// Requested scene ID does not exist in configuration.
    SceneNotFound { scene_id: String },
    /// Multiple windows on the target monitor matched a binding without disambiguation (AT-10).
    AmbiguousBindingSameMonitor {
        binding_id: String,
        candidate_hwnds: Vec<u64>,
    },
    /// A window on the target monitor matched, but another monitor also has a matching window (Section 4.2 #8).
    AmbiguousBindingCrossMonitor {
        binding_id: String,
        target_hwnd: u64,
    },
    /// Window is disabled because a modal dialog is open; skipped minimization (FR-17, AT-26).
    SkippedModalDisabledWindow {
        binding_id: String,
        hwnd: u64,
    },
    /// Process is elevated (Admin) or access was denied (Section 8.2).
    SkippedElevatedWindow {
        binding_id: String,
        hwnd: u64,
    },
    /// Window is not responding (`IsHungAppWindow`, AT-14).
    HungWindowDetected {
        binding_id: String,
        hwnd: u64,
    },
    /// Window hid itself to system tray (`!is_visible && !is_iconic`) after minimize (Section 8.2).
    HiddenToTrayOnMinimize {
        binding_id: String,
        hwnd: u64,
    },
    /// Window failed to minimize when checked by post-switch verification timer.
    MinimizeVerificationFailed {
        binding_id: String,
        hwnd: u64,
    },
    /// Window failed to restore when checked by post-switch verification timer.
    RestoreVerificationFailed {
        binding_id: String,
        hwnd: u64,
    },
}

#[derive(Debug, Clone, Default)]
pub struct PendingVerification {
    pub scene_id: String,
    pub target_monitor_id: String,
    pub minimized: Vec<(String, u64)>,
    pub restored: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct SwitchOutcome {
    /// True if the scene was already active and nothing was needed (Step 1 idempotency, AT-03).
    pub was_noop_same_scene: bool,
    /// True if the scene state transitioned to `Active(next_scene_id)`.
    pub switched: bool,
    pub minimized_hwnds: Vec<u64>,
    pub restored_hwnds: Vec<u64>,
    pub warnings: Vec<SwitchWarning>,
    pub verification: Option<PendingVerification>,
}

/// Determine the effective target monitor ID based on `config.target` and current launcher monitor.
pub fn resolve_effective_target_monitor_id(
    config: &AppConfig,
    launcher_monitor_id: Option<&str>,
) -> Option<String> {
    match config.target.monitor_policy {
        crate::config::MonitorPolicy::AllMonitors => launcher_monitor_id
            .map(|s| s.to_string())
            .or_else(|| config.target.monitor_id.clone())
            .or_else(|| Some("ALL_MONITORS".to_string())),
        crate::config::MonitorPolicy::Fixed => config.target.monitor_id.clone(),
        crate::config::MonitorPolicy::LauncherMonitor => launcher_monitor_id
            .map(|s| s.to_string())
            .or_else(|| config.target.monitor_id.clone()),
    }
}

/// Rank a binding by specificity so that specific rules (cached HWND, title keyword, monitor ID)
/// are resolved before catch-all (`TitleRuleType::Any`) bindings of the same executable.
fn binding_specificity_rank(binding: &crate::config::WindowBinding, resolver: &BindingResolver) -> u8 {
    let has_cached = resolver.cached_identity(&binding.id).is_some();
    let has_hwnd_hint = binding
        .instance_hint
        .as_ref()
        .and_then(|h| h.last_hwnd)
        .is_some();
    let has_specific_title =
        binding.match_rule.title_rule.rule_type != crate::config::TitleRuleType::Any;
    let has_monitor = binding.match_rule.monitor_id.is_some();

    match (has_cached || has_hwnd_hint, has_specific_title, has_monitor) {
        (true, _, _) => 0,
        (false, true, true) => 1,
        (false, true, false) => 2,
        (false, false, true) => 3,
        (false, false, false) => 4,
    }
}

/// Execute the 10-step `switch_scene` algorithm (Section 5.1).
pub fn execute_scene_switch(
    next_scene_id: &str,
    config: &AppConfig,
    state_mgr: &mut SceneStateManager,
    resolver: &mut BindingResolver,
    recovery: &mut RecoveryManager,
    sys: &mut dyn WindowSystem,
    launcher_monitor_id: Option<&str>,
) -> SwitchOutcome {
    let mut warnings = Vec::new();

    // Verify scene exists
    if !config.scenes.iter().any(|s| s.id == next_scene_id) {
        warnings.push(SwitchWarning::SceneNotFound {
            scene_id: next_scene_id.to_string(),
        });
        return SwitchOutcome {
            was_noop_same_scene: false,
            switched: false,
            minimized_hwnds: Vec::new(),
            restored_hwnds: Vec::new(),
            warnings,
            verification: None,
        };
    }

    let already_active = state_mgr.is_already_active(next_scene_id);
    let is_all_monitors =
        config.target.monitor_policy == crate::config::MonitorPolicy::AllMonitors;

    let monitors = sys.monitors();
    let connected_monitor_ids: Vec<String> =
        monitors.iter().map(|m| m.monitor_id.clone()).collect();

    let target_mon_id = if is_all_monitors {
        "ALL_MONITORS".to_string()
    } else {
        let Some(mon_id) = resolve_effective_target_monitor_id(config, launcher_monitor_id) else {
            if already_active {
                return SwitchOutcome {
                    was_noop_same_scene: true,
                    switched: false,
                    minimized_hwnds: Vec::new(),
                    restored_hwnds: Vec::new(),
                    warnings,
                    verification: None,
                };
            }
            warnings.push(SwitchWarning::TargetMonitorNotConfigured);
            return SwitchOutcome {
                was_noop_same_scene: false,
                switched: false,
                minimized_hwnds: Vec::new(),
                restored_hwnds: Vec::new(),
                warnings,
                verification: None,
            };
        };

        if !monitors.iter().any(|m| m.monitor_id == mon_id) {
            if already_active {
                return SwitchOutcome {
                    was_noop_same_scene: true,
                    switched: false,
                    minimized_hwnds: Vec::new(),
                    restored_hwnds: Vec::new(),
                    warnings,
                    verification: None,
                };
            }
            warnings.push(SwitchWarning::TargetMonitorDisconnected {
                monitor_id: mon_id,
            });
            return SwitchOutcome {
                was_noop_same_scene: false,
                switched: false,
                minimized_hwnds: Vec::new(),
                restored_hwnds: Vec::new(),
                warnings,
                verification: None,
            };
        }
        mon_id
    };

    // Step 2: Enumerate and re-validate registered windows
    let all_windows = sys.enumerate_windows();

    // First, determine which HWNDs are protected in `next_scene_id` (either belonging to `next` or `always_visible`).
    // This guarantees common windows shared across scenes are never minimized (FR-07, AT-07).
    let mut protected_hwnds_in_next: HashSet<u64> = HashSet::new();
    let mut resolved_bindings: Vec<(String, WindowInstance, bool, bool)> = Vec::new(); // (binding_id, win, in_next, always_visible)
    let mut claimed_hwnds: HashSet<u64> = HashSet::new();

    // Resolve bindings in order of specificity so keyword/monitor-pinned bindings claim their windows first
    let mut ordered_bindings: Vec<&crate::config::WindowBinding> =
        config.bindings.iter().filter(|b| b.enabled).collect();
    ordered_bindings.sort_by_key(|b| binding_specificity_rank(b, resolver));

    for binding in ordered_bindings {
        let in_next = binding.scene_ids.iter().any(|id| id == next_scene_id);
        let resolution = if is_all_monitors {
            resolver.resolve_binding_multi_monitor(
                binding,
                &all_windows,
                &connected_monitor_ids,
                &claimed_hwnds,
            )
        } else {
            resolver.resolve_binding_single_monitor(
                binding,
                &all_windows,
                &target_mon_id,
                &claimed_hwnds,
            )
        };

        match resolution {
            BindingResolution::Resolved(win) => {
                claimed_hwnds.insert(win.hwnd);
                if in_next || binding.always_visible {
                    protected_hwnds_in_next.insert(win.hwnd);
                }
                resolved_bindings.push((binding.id.clone(), win, in_next, binding.always_visible));
            }
            BindingResolution::MovedToOtherMonitor(_win) => {
                // User manually moved window to another monitor in single-monitor mode; do not touch it (Section 8.2)
            }
            BindingResolution::AmbiguousSameMonitor(candidates) => {
                warnings.push(SwitchWarning::AmbiguousBindingSameMonitor {
                    binding_id: binding.id.clone(),
                    candidate_hwnds: candidates.iter().map(|c| c.hwnd).collect(),
                });
            }
            BindingResolution::AmbiguousCrossMonitor {
                target_candidate, ..
            } => {
                warnings.push(SwitchWarning::AmbiguousBindingCrossMonitor {
                    binding_id: binding.id.clone(),
                    target_hwnd: target_candidate.hwnd,
                });
            }
            BindingResolution::NotFound => {}
        }
    }

    // Step 3 & Step 5: Compute `to_minimize` = visible, non-iconic registered windows on target monitor(s)
    // minus `next` scene windows minus `always_visible`, filtering out modal disabled windows (FR-17) and elevated windows.
    let mut to_minimize: Vec<(String, WindowInstance)> = Vec::new();
    let mut seen_minimize_hwnds: HashSet<u64> = HashSet::new();

    for (binding_id, win, in_next, always_visible) in &resolved_bindings {
        if *in_next || *always_visible || protected_hwnds_in_next.contains(&win.hwnd) {
            continue;
        }
        let on_valid_monitor = if is_all_monitors {
            connected_monitor_ids.iter().any(|m| m == &win.monitor_id)
        } else {
            win.monitor_id == target_mon_id
        };
        if !on_valid_monitor || !win.is_visible || win.is_iconic {
            continue;
        }
        if !seen_minimize_hwnds.insert(win.hwnd) {
            continue;
        }

        // Step 5 (FR-17, AT-26): Never minimize a window that is disabled (modal dialog open, e.g. order confirmation)
        if !win.is_enabled {
            log_warn(
                "switch",
                &format!(
                    "Skipping minimize for disabled (modal) window 0x{:X} (binding '{}', title={})",
                    win.hwnd,
                    binding_id,
                    mask_title(&win.title)
                ),
            );
            warnings.push(SwitchWarning::SkippedModalDisabledWindow {
                binding_id: binding_id.clone(),
                hwnd: win.hwnd,
            });
            continue;
        }

        if win.is_elevated {
            log_warn(
                "switch",
                &format!(
                    "Skipping elevated window 0x{:X} (binding '{}')",
                    win.hwnd, binding_id
                ),
            );
            warnings.push(SwitchWarning::SkippedElevatedWindow {
                binding_id: binding_id.clone(),
                hwnd: win.hwnd,
            });
            continue;
        }

        if win.is_hung {
            warnings.push(SwitchWarning::HungWindowDetected {
                binding_id: binding_id.clone(),
                hwnd: win.hwnd,
            });
        }

        to_minimize.push((binding_id.clone(), win.clone()));
    }

    // Step 4: Compute `to_restore` = windows belonging to `next` (not `always_visible`)
    // that are currently `IsIconic`. Also compute `to_bring_to_front` = windows belonging to `next`
    // that are already open (non-iconic) so they are brought above previously open/unregistered apps.
    let mut to_restore: Vec<(String, WindowInstance, usize)> = Vec::new();
    let mut to_bring_to_front: Vec<(String, WindowInstance, usize)> = Vec::new();
    let mut seen_restore_hwnds: HashSet<u64> = HashSet::new();

    if config.launcher.restore_all_on_switch {
        // Mode A (Default): Restore ALL windows registered to `next` that are currently iconic,
        // regardless of whether they were minimized by Kiri or manually by the user.
        for (binding_id, win, in_next, always_visible) in &resolved_bindings {
            if !*in_next || *always_visible {
                continue;
            }
            let on_valid_monitor = if is_all_monitors {
                connected_monitor_ids.iter().any(|m| m == &win.monitor_id)
            } else {
                win.monitor_id == target_mon_id
            };
            if on_valid_monitor {
                if win.is_iconic {
                    if seen_restore_hwnds.insert(win.hwnd) {
                        if win.is_hung {
                            warnings.push(SwitchWarning::HungWindowDetected {
                                binding_id: binding_id.clone(),
                                hwnd: win.hwnd,
                            });
                        }
                        let z = recovery
                            .actions()
                            .get(&win.hwnd)
                            .map(|a| a.z_order_at_minimize)
                            .unwrap_or(win.z_order);
                        to_restore.push((binding_id.clone(), win.clone(), z));
                    }
                } else if win.is_visible {
                    let z = recovery
                        .actions()
                        .get(&win.hwnd)
                        .map(|a| a.z_order_at_minimize)
                        .unwrap_or(win.z_order);
                    to_bring_to_front.push((binding_id.clone(), win.clone(), z));
                }
            }
        }
    }

    // Also include any recovery actions recorded on target monitor(s) that are still iconic
    for binding in &config.bindings {
        if !binding.enabled || binding.always_visible {
            continue;
        }
        if !binding.scene_ids.iter().any(|id| id == next_scene_id) {
            continue;
        }

        // Look up ManagedAction by binding_id or resolved HWND
        let matching_actions: Vec<_> = recovery
            .actions()
            .values()
            .filter(|a| {
                a.binding_id == binding.id
                    && (config.launcher.restore_all_on_switch || a.is_restorable_by_scene_switch())
                    && (is_all_monitors || a.monitor_id == target_mon_id)
            })
            .cloned()
            .collect();

        for action in matching_actions {
            if !seen_restore_hwnds.insert(action.hwnd) {
                continue;
            }
            if let Some(live) = sys.query_window(action.hwnd) {
                let on_valid_monitor = if is_all_monitors {
                    connected_monitor_ids.iter().any(|m| m == &live.monitor_id)
                } else {
                    live.monitor_id == target_mon_id
                };
                // Verify runtime identity `(hwnd, pid, process_start_time)`, monitor attribution, and `is_iconic`
                if live.matches_runtime_identity(
                    action.hwnd,
                    action.pid,
                    action.process_start_time,
                ) && on_valid_monitor
                    && live.is_iconic
                {
                    if live.is_hung {
                        warnings.push(SwitchWarning::HungWindowDetected {
                            binding_id: binding.id.clone(),
                            hwnd: live.hwnd,
                        });
                    }
                    to_restore.push((binding.id.clone(), live, action.z_order_at_minimize));
                } else if !live.matches_runtime_identity(
                    action.hwnd,
                    action.pid,
                    action.process_start_time,
                ) {
                    recovery.invalidate_hwnd(action.hwnd);
                }
            }
        }
    }

    // Step 1 idempotency check (AT-03): If `state == Active(next)` and no newly reopened windows need
    // minimizing/restoring, return a clean no-op without issuing any OS commands.
    if already_active && to_minimize.is_empty() && to_restore.is_empty() {
        return SwitchOutcome {
            was_noop_same_scene: true,
            switched: false,
            minimized_hwnds: Vec::new(),
            restored_hwnds: Vec::new(),
            warnings: Vec::new(),
            verification: None,
        };
    }

    // Sort `to_restore` by recorded Z-order descending (bottom-most window restored first, Section 5.3)
    to_restore.sort_by(|a, b| b.2.cmp(&a.2));

    // Step 6: Write `to_minimize` as `Pending` to write-ahead journal BEFORE issuing OS minimize commands
    if !to_minimize.is_empty() {
        recovery.record_pending_minimizes(&to_minimize);
    }

    // Check if the current foreground window is among `to_minimize` (Section 5.4, FR-18, AT-25)
    let foreground_hwnd = sys.get_foreground_window();
    let mut deferred_foreground_minimize: Option<(String, WindowInstance)> = None;

    let mut minimized_hwnds = Vec::new();
    let mut verification_minimized = Vec::new();

    // Step 7a: Minimize all non-foreground windows in `to_minimize` using `SW_SHOWMINNOACTIVE`
    for (binding_id, win) in to_minimize {
        if foreground_hwnd == Some(win.hwnd) {
            deferred_foreground_minimize = Some((binding_id, win));
            continue;
        }
        match sys.show_window_async(win.hwnd, ShowCmd::MinimizeNoActivate) {
            Ok(()) => {
                recovery.mark_minimize_issued(win.hwnd);
                minimized_hwnds.push(win.hwnd);
                verification_minimized.push((binding_id, win.hwnd));
            }
            Err(e) => {
                log_warn(
                    "switch",
                    &format!("Minimize failed for 0x{:X}: {}", win.hwnd, e),
                );
                recovery.invalidate_hwnd(win.hwnd);
            }
        }
    }

    // Step 8: Restore `to_restore` windows from bottom to top of Z-order
    let mut restored_hwnds = Vec::new();
    let mut verification_restored = Vec::new();
    let total_restore = to_restore.len();

    for (idx, (binding_id, win, _z)) in to_restore.iter().enumerate() {
        // Section 5.3: Always re-verify `is_iconic` immediately before sending restore command
        let still_iconic = sys
            .query_window(win.hwnd)
            .map(|w| w.is_iconic)
            .unwrap_or(false);
        if !still_iconic {
            recovery.invalidate_hwnd(win.hwnd);
            continue;
        }

        let is_topmost_restore = idx + 1 == total_restore;
        let cmd = if config.launcher.focus_on_switch
            && is_topmost_restore
            && deferred_foreground_minimize.is_none()
        {
            ShowCmd::RestoreActivate
        } else {
            ShowCmd::RestoreNoActivate
        };

        recovery.mark_restoring_by_us(win.hwnd);
        match sys.show_window_async(win.hwnd, cmd) {
            Ok(()) => {
                restored_hwnds.push(win.hwnd);
                verification_restored.push((binding_id.clone(), win.hwnd));
            }
            Err(e) => {
                log_warn(
                    "switch",
                    &format!("Restore failed for 0x{:X}: {}", win.hwnd, e),
                );
            }
        }
    }

    // Step 7b (Section 5.4, FR-18, AT-25): If the foreground window was in `to_minimize`,
    // minimize it AFTER restoring next-scene windows using `SW_MINIMIZE` (`MinimizeActivateNext`)
    // so keyboard focus never stays trapped on an invisible minimized window.
    if let Some((binding_id, fg_win)) = &deferred_foreground_minimize {
        match sys.show_window_async(fg_win.hwnd, ShowCmd::MinimizeActivateNext) {
            Ok(()) => {
                recovery.mark_minimize_issued(fg_win.hwnd);
                minimized_hwnds.push(fg_win.hwnd);
                verification_minimized.push((binding_id.clone(), fg_win.hwnd));
            }
            Err(e) => {
                log_warn(
                    "switch",
                    &format!("Foreground minimize failed for 0x{:X}: {}", fg_win.hwnd, e),
                );
                recovery.invalidate_hwnd(fg_win.hwnd);
            }
        }
    }

    // Step 8b: Bring all next-scene windows (restored + already non-iconic) to the top of Z-order
    // in bottom-to-top order so they appear above previously open/unregistered apps,
    // and activate the topmost window so it doesn't get obscured behind active apps.
    let mut all_target_scene_windows: Vec<(u64, usize, bool)> = Vec::new(); // (hwnd, z_order, is_hung)
    for (_bid, win, z) in &to_restore {
        if restored_hwnds.contains(&win.hwnd) {
            all_target_scene_windows.push((win.hwnd, *z, win.is_hung));
        }
    }
    for (_bid, win, z) in &to_bring_to_front {
        all_target_scene_windows.push((win.hwnd, *z, win.is_hung));
    }

    // Sort descending by Z-order (largest Z = bottom-most first, so topmost window is brought to top last)
    all_target_scene_windows.sort_by(|a, b| b.1.cmp(&a.1));

    for (h, _z, _is_hung) in &all_target_scene_windows {
        sys.bring_to_top(*h);
    }

    // Determine the topmost candidate to focus
    let topmost_candidate = all_target_scene_windows
        .iter()
        .rev()
        .find(|(_, _, is_hung)| !*is_hung)
        .or_else(|| all_target_scene_windows.last());

    if let Some((top_hwnd, _, _)) = topmost_candidate {
        if config.launcher.focus_on_switch || deferred_foreground_minimize.is_some() {
            sys.set_foreground_window(*top_hwnd);
        }
    }

    let _ = recovery.persist_journal();

    // Step 9: Confirm `state = Active(next)`
    state_mgr.set_active(next_scene_id.to_string());
    log_info(
        "switch",
        &format!(
            "Switched to scene '{}' on monitor '{}' (minimized={}, restored={}, warnings={})",
            next_scene_id,
            target_mon_id,
            minimized_hwnds.len(),
            restored_hwnds.len(),
            warnings.len()
        ),
    );

    let verification = if !verification_minimized.is_empty() || !verification_restored.is_empty() {
        Some(PendingVerification {
            scene_id: next_scene_id.to_string(),
            target_monitor_id: target_mon_id,
            minimized: verification_minimized,
            restored: verification_restored,
        })
    } else {
        None
    };

    SwitchOutcome {
        was_noop_same_scene: false,
        switched: true,
        minimized_hwnds,
        restored_hwnds,
        warnings,
        verification,
    }
}

/// Step 10 of `switch_scene` (called ~500ms after `execute_scene_switch` via a one-shot timer):
/// Verifies that minimized and restored windows reached their expected state, detects apps that
/// hid themselves to the system tray (`!is_visible && !is_iconic`, Section 8.2), and ensures
/// keyboard focus is not stranded on a minimized window (Section 5.4, FR-18).
pub fn verify_switch_results(
    pending: &PendingVerification,
    recovery: &mut RecoveryManager,
    sys: &mut dyn WindowSystem,
) -> Vec<SwitchWarning> {
    let mut warnings = Vec::new();
    let mut journal_dirty = false;

    for (binding_id, hwnd) in &pending.minimized {
        if let Some(live) = sys.query_window(*hwnd) {
            if !live.is_visible && !live.is_iconic {
                // Section 8.2: App hid itself to system tray upon minimize
                log_warn(
                    "switch",
                    &format!(
                        "Window 0x{:X} (binding '{}') hid itself upon minimize (tray app); marking HiddenToTray",
                        hwnd, binding_id
                    ),
                );
                recovery.mark_hidden_to_tray(*hwnd);
                journal_dirty = true;
                warnings.push(SwitchWarning::HiddenToTrayOnMinimize {
                    binding_id: binding_id.clone(),
                    hwnd: *hwnd,
                });
            } else if !live.is_iconic {
                log_warn(
                    "switch",
                    &format!(
                        "Window 0x{:X} (binding '{}') did not complete minimize",
                        hwnd, binding_id
                    ),
                );
                warnings.push(SwitchWarning::MinimizeVerificationFailed {
                    binding_id: binding_id.clone(),
                    hwnd: *hwnd,
                });
            }
        }
    }

    for (binding_id, hwnd) in &pending.restored {
        if let Some(live) = sys.query_window(*hwnd) {
            if live.is_iconic {
                log_warn(
                    "switch",
                    &format!(
                        "Window 0x{:X} (binding '{}') remained iconic after restore",
                        hwnd, binding_id
                    ),
                );
                warnings.push(SwitchWarning::RestoreVerificationFailed {
                    binding_id: binding_id.clone(),
                    hwnd: *hwnd,
                });
            }
        }
    }

    // Section 5.4 (FR-18): Verify the current foreground window is not still a minimized window
    if let Some(fg_hwnd) = sys.get_foreground_window() {
        let fg_is_minimized = sys
            .query_window(fg_hwnd)
            .map(|w| w.is_iconic || !w.is_visible)
            .unwrap_or(false);
        if fg_is_minimized {
            // Move focus to the topmost restored window or topmost visible window on target monitor
            let fallback_hwnd = pending
                .restored
                .last()
                .map(|(_, h)| *h)
                .or_else(|| {
                    sys.enumerate_windows()
                        .into_iter()
                        .find(|w| w.is_visible && !w.is_iconic)
                        .map(|w| w.hwnd)
                });
            if let Some(safe_hwnd) = fallback_hwnd {
                let _ = sys.set_foreground_window(safe_hwnd);
            }
        }
    }

    if journal_dirty {
        let _ = recovery.persist_journal();
    }

    warnings
}
