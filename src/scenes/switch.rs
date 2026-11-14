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

mod trace;

pub use self::trace::SwitchSource;
use self::trace::{binding_event, window_state, PresentationPhase, SwitchTrace};
use crate::config::{AppConfig, MonitorPolicy, Scene, TargetConfig};
use crate::diagnostics::{log_info, log_warn, mask_title};
use crate::platform::{ShowCmd, WindowInstance, WindowRaise, WindowSystem};
use crate::recovery::RecoveryManager;
use crate::scenes::bindings::{BindingResolution, BindingResolver, BoundRuntimeIdentity};
use crate::scenes::manager::SceneStateManager;
use serde_json::json;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchWarning {
    /// Target monitor is not configured (`target.monitor_id` is None).
    TargetMonitorNotConfigured,
    /// Target monitor is currently disconnected.
    TargetMonitorDisconnected { monitor_id: String },
    /// Requested scene ID does not exist in configuration.
    SceneNotFound { scene_id: String },
    /// Requested variant no longer exists in the scene.
    SceneVariantNotFound { scene_id: String },
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
    /// Windows did not accept a Z-order or focus request on the verification pass.
    ScenePresentationFailed { scene_id: String },
}

#[derive(Debug, Clone)]
pub struct PendingVerification {
    pub scene_id: String,
    pub target_monitor_id: String,
    pub minimized: Vec<(String, u64)>,
    pub restored: Vec<(String, u64)>,
    presentation: Option<PendingPresentation>,
    trace: SwitchTrace,
    trace_targets: Vec<u64>,
}

#[derive(Debug, Clone)]
struct PendingPresentation {
    windows: Vec<BoundRuntimeIdentity>,
    focus: bool,
    input_tick: u32,
}

impl PendingVerification {
    pub fn cancel_presentation_retry(&mut self) {
        self.trace
            .record("presentation_cancelled", || json!({"reason": "settings_changed"}));
        self.trace.flush();
        self.presentation = None;
    }

    pub fn skip_verification(&self, reason: &str) {
        self.trace
            .record("verification_skipped", || json!({"reason": reason}));
        self.trace.flush();
    }

    pub fn record_timer_failure(&self, error: u32) {
        self.trace
            .record("verification_timer_failed", || json!({"win32_error": error}));
        self.trace.flush();
    }
}

#[derive(Debug, Clone)]
pub struct SwitchOutcome {
    /// True if the scene was already active and needed no window state or presentation changes.
    pub was_noop_same_scene: bool,
    /// True if the requested scene was applied, including a reapplication of the active scene.
    pub switched: bool,
    pub minimized_hwnds: Vec<u64>,
    pub restored_hwnds: Vec<u64>,
    pub warnings: Vec<SwitchWarning>,
    pub verification: Option<PendingVerification>,
}

/// Resolve a target monitor ID. For all-monitor scenes, this ID is only used for the launcher profile.
pub fn resolve_effective_target_monitor_id(
    target: &TargetConfig,
    launcher_monitor_id: Option<&str>,
) -> Option<String> {
    match target.monitor_policy {
        MonitorPolicy::AllMonitors => launcher_monitor_id
            .map(|s| s.to_string())
            .or_else(|| target.monitor_id.clone())
            .or_else(|| Some("ALL_MONITORS".to_string())),
        MonitorPolicy::Fixed => target.monitor_id.clone(),
        MonitorPolicy::LauncherMonitor => launcher_monitor_id
            .map(|s| s.to_string())
            .or_else(|| target.monitor_id.clone()),
    }
}

fn resolve_scene_selection<'a>(
    config: &'a AppConfig,
    scene_id: &str,
    variant_id: Option<&str>,
) -> Result<(&'a Scene, Option<&'a str>), SwitchWarning> {
    let scene = config
        .scene(scene_id)
        .ok_or_else(|| SwitchWarning::SceneNotFound {
            scene_id: scene_id.to_owned(),
        })?;
    let variant = if let Some(id) = variant_id {
        Some(
            scene
                .variants
                .iter()
                .find(|variant| variant.id == id)
                .ok_or_else(|| SwitchWarning::SceneVariantNotFound {
                    scene_id: scene_id.to_owned(),
                })?,
        )
    } else {
        scene.preferred_variant()
    };
    Ok((scene, variant.map(|variant| variant.id.as_str())))
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

fn scene_focus_target(windows: &[&WindowInstance]) -> Option<u64> {
    windows
        .iter()
        .rev()
        .find(|win| win.is_enabled && !win.is_hung)
        .map(|win| win.hwnd)
}

/// Execute the 10-step `switch_scene` algorithm (Section 5.1).
#[cfg(test)]
pub fn execute_scene_switch(
    next_scene_id: &str,
    config: &AppConfig,
    state_mgr: &mut SceneStateManager,
    resolver: &mut BindingResolver,
    recovery: &mut RecoveryManager,
    sys: &mut dyn WindowSystem,
    launcher_monitor_id: Option<&str>,
) -> SwitchOutcome {
    execute_scene_switch_from(
        next_scene_id,
        None,
        config,
        state_mgr,
        resolver,
        recovery,
        sys,
        launcher_monitor_id,
        SwitchSource::Unspecified,
    )
}

struct SwitchPlan {
    target_mon_id: String,
    to_minimize: Vec<(String, WindowInstance)>,
    to_restore: Vec<(String, WindowInstance, usize)>,
    to_bring_to_front: Vec<(WindowInstance, usize)>,
    was_noop_same_scene: bool,
    warnings: Vec<SwitchWarning>,
    stale_actions: Vec<u64>,
    unchanged: Vec<(WindowInstance, &'static str)>,
}

pub struct SwitchPreview {
    pub restore: Vec<WindowInstance>,
    pub minimize: Vec<WindowInstance>,
    pub unchanged: Vec<(WindowInstance, &'static str)>,
    pub warnings: Vec<SwitchWarning>,
}

pub fn preview_scene_switch(
    next_scene_id: &str,
    variant_id: Option<&str>,
    config: &AppConfig,
    mut resolver: BindingResolver,
    recovery: &RecoveryManager,
    sys: &dyn WindowSystem,
    launcher_monitor_id: Option<&str>,
) -> SwitchPreview {
    let context = PlanContext {
        trace: &SwitchTrace::silent(),
        input_tick: None,
        preview: true,
        already_active: false,
    };
    let plan =
        resolve_scene_selection(config, next_scene_id, variant_id).and_then(|(scene, variant)| {
            plan_scene_switch(
                scene,
                variant,
                config,
                &mut resolver,
                recovery,
                sys,
                launcher_monitor_id,
                context,
            )
        });
    match plan {
        Ok(plan) => SwitchPreview {
            restore: plan.to_restore.into_iter().map(|(_, win, _)| win).collect(),
            minimize: plan.to_minimize.into_iter().map(|(_, win)| win).collect(),
            unchanged: plan.unchanged,
            warnings: plan.warnings,
        },
        Err(warning) => SwitchPreview {
            restore: Vec::new(),
            minimize: Vec::new(),
            unchanged: Vec::new(),
            warnings: vec![warning],
        },
    }
}

struct PlanContext<'a> {
    trace: &'a SwitchTrace,
    input_tick: Option<u32>,
    preview: bool,
    already_active: bool,
}

fn plan_scene_switch(
    scene: &Scene,
    variant_id: Option<&str>,
    config: &AppConfig,
    resolver: &mut BindingResolver,
    recovery: &RecoveryManager,
    sys: &dyn WindowSystem,
    launcher_monitor_id: Option<&str>,
    context: PlanContext<'_>,
) -> Result<SwitchPlan, SwitchWarning> {
    let PlanContext {
        trace,
        input_tick,
        preview,
        already_active,
    } = context;
    let mut warnings = Vec::new();
    let mut stale_actions = Vec::new();
    let mut unchanged = Vec::new();

    let target = scene.target(&config.target);
    let in_selection = |binding: &crate::config::WindowBinding| {
        binding.belongs_to_selection(&scene.id, variant_id)
    };
    let is_all_monitors = target.monitor_policy == MonitorPolicy::AllMonitors;
    let mut outside_resolver = (preview && !is_all_monitors).then(BindingResolver::new);
    let monitors = sys.monitors();
    let connected_monitor_ids: Vec<String> = monitors.iter().map(|m| m.monitor_id.clone()).collect();
    let target_mon_id = if is_all_monitors {
        "ALL_MONITORS".to_string()
    } else {
        let mon_id = resolve_effective_target_monitor_id(target, launcher_monitor_id)
            .ok_or(SwitchWarning::TargetMonitorNotConfigured)?;
        if !connected_monitor_ids.contains(&mon_id) {
            return Err(SwitchWarning::TargetMonitorDisconnected { monitor_id: mon_id });
        }
        mon_id
    };

    // Step 2: Enumerate and re-validate registered windows
    let all_windows = sys.enumerate_windows();
    trace.record("target", || {
        json!({
            "monitor": target_mon_id,
            "connected_monitors": connected_monitor_ids,
            "enumerated_windows": all_windows.len(),
            "last_input_tick": input_tick,
        })
    });

    // First, determine which HWNDs are protected in `next_scene_id` (either belonging to `next` or `always_visible`).
    // This guarantees common windows shared across scenes are never minimized (FR-07, AT-07).
    let mut protected_hwnds_in_next: HashSet<u64> = HashSet::new();
    let mut resolved_bindings: Vec<(String, WindowInstance, bool, bool)> = Vec::new(); // (binding_id, win, in_next, always_visible)
    let mut claimed_hwnds: HashSet<u64> = HashSet::new();

    // Resolve bindings in order of specificity so keyword/monitor-pinned bindings claim their windows first
    let mut ordered_bindings = Vec::new();
    for binding in &config.bindings {
        if binding.enabled {
            ordered_bindings.push(binding);
        } else if trace.is_enabled() && in_selection(binding) {
            trace.record("binding", || {
                json!({
                    "binding": binding.id,
                    "exe": crate::diagnostics::executable_name(&binding.match_rule.exe_path),
                    "in_scene": true,
                    "result": "disabled",
                })
            });
        }
    }
    ordered_bindings.sort_by_key(|b| binding_specificity_rank(b, resolver));

    for binding in ordered_bindings {
        let in_next = in_selection(binding);
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

        trace.record("binding", || {
            binding_event(binding, in_next, &resolution, &all_windows, &claimed_hwnds)
        });
        match resolution {
            BindingResolution::Resolved(win) => {
                claimed_hwnds.insert(win.hwnd);
                if in_next || binding.always_visible {
                    protected_hwnds_in_next.insert(win.hwnd);
                }
                resolved_bindings.push((binding.id.clone(), win, in_next, binding.always_visible));
            }
            BindingResolution::MovedToOtherMonitor(win) => {
                if preview { unchanged.push((win, "操作する画面の外")); }
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
            BindingResolution::NotFound => {
                if let Some(outside_resolver) = &mut outside_resolver {
                    if let BindingResolution::Resolved(win) = outside_resolver
                        .resolve_binding_multi_monitor(
                            binding,
                            &all_windows,
                            &connected_monitor_ids,
                            &claimed_hwnds,
                        )
                    {
                        if win.monitor_id != target_mon_id {
                            unchanged.push((win, "操作する画面の外"));
                        }
                    }
                }
            }
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
            if !preview { log_warn(
                "switch",
                &format!(
                    "Skipping minimize for disabled (modal) window 0x{:X} (binding '{}', title={})",
                    win.hwnd,
                    binding_id,
                    mask_title(&win.title)
                ),
            ); }
            warnings.push(SwitchWarning::SkippedModalDisabledWindow {
                binding_id: binding_id.clone(),
                hwnd: win.hwnd,
            });
            continue;
        }

        if win.is_elevated {
            if !preview { log_warn(
                "switch",
                &format!(
                    "Skipping elevated window 0x{:X} (binding '{}')",
                    win.hwnd, binding_id
                ),
            ); }
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
    let mut to_bring_to_front: Vec<(WindowInstance, usize)> = Vec::new();
    let mut seen_restore_hwnds: HashSet<u64> = HashSet::new();

    for (binding_id, win, in_next, always_visible) in &resolved_bindings {
        if !*in_next || *always_visible {
            continue;
        }
        let on_valid_monitor = if is_all_monitors {
            connected_monitor_ids.iter().any(|m| m == &win.monitor_id)
        } else {
            win.monitor_id == target_mon_id
        };
        if !on_valid_monitor {
            trace.record("presentation_candidate_skipped", || {
                json!({
                    "binding": binding_id, "hwnd": win.hwnd,
                    "reason": "outside_target_monitor", "monitor": win.monitor_id,
                })
            });
            continue;
        }
        let z = recovery
            .actions()
            .get(&win.hwnd)
            .map(|a| a.z_order_at_minimize)
            .unwrap_or(win.z_order);
        if win.is_iconic {
            if config.launcher.restore_all_on_switch && seen_restore_hwnds.insert(win.hwnd) {
                if win.is_hung {
                    warnings.push(SwitchWarning::HungWindowDetected {
                        binding_id: binding_id.clone(),
                        hwnd: win.hwnd,
                    });
                }
                to_restore.push((binding_id.clone(), win.clone(), z));
            }
        } else if win.is_visible {
            to_bring_to_front.push((win.clone(), z));
        }
    }

    // Also include any recovery actions recorded on target monitor(s) that are still iconic
    for binding in &config.bindings {
        if !binding.enabled || binding.always_visible {
            continue;
        }
        if !in_selection(binding) {
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
                    stale_actions.push(action.hwnd);
                }
            }
        }
    }

    if preview {
        let mut seen: HashSet<u64> = to_minimize
            .iter()
            .map(|(_, win)| win.hwnd)
            .chain(to_restore.iter().map(|(_, win, _)| win.hwnd))
            .collect();
        unchanged.retain(|(win, _)| seen.insert(win.hwnd));
        for (_, win, in_next, always_visible) in resolved_bindings {
            if seen.insert(win.hwnd) {
                let reason = if always_visible { "常に表示" }
                    else if win.is_iconic && in_next { "最小化したまま" }
                    else if !win.is_enabled { "ダイアログを表示中" }
                    else if win.is_elevated { "管理者権限で実行中" }
                    else if win.is_visible && in_next { "表示を維持" }
                    else { "現在の状態を維持" };
                unchanged.push((win, reason));
            }
        }
    }

    let was_noop_same_scene = already_active
        && to_minimize.is_empty()
        && to_restore.is_empty()
        && !needs_scene_presentation(
            &to_bring_to_front,
            &all_windows,
            config.launcher.focus_on_switch,
            sys,
        );

    Ok(SwitchPlan {
        target_mon_id,
        to_minimize,
        to_restore,
        to_bring_to_front,
        was_noop_same_scene,
        warnings,
        stale_actions,
        unchanged,
    })
}

pub fn execute_scene_switch_from(
    next_scene_id: &str,
    requested_variant_id: Option<&str>,
    config: &AppConfig,
    state_mgr: &mut SceneStateManager,
    resolver: &mut BindingResolver,
    recovery: &mut RecoveryManager,
    sys: &mut dyn WindowSystem,
    launcher_monitor_id: Option<&str>,
    source: SwitchSource,
) -> SwitchOutcome {
    let input_tick = sys.last_input_tick();
    let trace = SwitchTrace::new();
    let selection = resolve_scene_selection(config, next_scene_id, requested_variant_id);
    let variant_id = selection.as_ref().ok().and_then(|(_, variant)| *variant);
    let target = config
        .scene(next_scene_id)
        .map_or(&config.target, |scene| scene.target(&config.target));
    trace.record("started", || {
        json!({
            "from_scene": state_mgr.active_scene_id(),
            "from_variant": state_mgr.active_variant_id(),
            "to_scene": next_scene_id,
            "to_variant": variant_id,
            "source": source,
            "monitor_policy": target.monitor_policy,
            "configured_monitor": target.monitor_id,
            "launcher_monitor": launcher_monitor_id,
            "focus_on_switch": config.launcher.focus_on_switch,
            "restore_all_on_switch": config.launcher.restore_all_on_switch,
            "foreground": sys.foreground_state(),
        })
    });
    let already_active = state_mgr.is_already_active(next_scene_id, variant_id);
    let context = PlanContext {
        trace: &trace,
        input_tick,
        preview: false,
        already_active,
    };
    let plan = selection.and_then(|(scene, variant)| {
        plan_scene_switch(
            scene,
            variant,
            config,
            resolver,
            recovery,
            sys,
            launcher_monitor_id,
            context,
        )
    });
    let SwitchPlan {
        target_mon_id,
        to_minimize,
        mut to_restore,
        mut to_bring_to_front,
        was_noop_same_scene,
        warnings,
        stale_actions,
        ..
    } = match plan {
        Ok(plan) => plan,
        Err(warning) => {
            trace.record(
                "finished",
                || json!({"reason": format!("{warning:?}"), "already_active": already_active}),
            );
            return SwitchOutcome {
                was_noop_same_scene: false,
                switched: false,
                minimized_hwnds: Vec::new(),
                restored_hwnds: Vec::new(),
                warnings: vec![warning],
                verification: None,
            };
        }
    };
    let journal_dirty = !stale_actions.is_empty() || !to_minimize.is_empty() || !to_restore.is_empty();
    for hwnd in stale_actions {
        recovery.invalidate_hwnd(hwnd);
    }

    // AT-03: Reselecting the active scene can change stacking even without minimizing or restoring.
    if was_noop_same_scene {
        trace.record("finished", || json!({"reason": "already_active"}));
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
    let stays_on_target = |win: &WindowInstance, live: &WindowInstance| {
        win.monitor_id == live.monitor_id && (target_mon_id == "ALL_MONITORS" || live.monitor_id == target_mon_id)
    };
    trace.record("plan", || {
        json!({
            "minimize": to_minimize.iter().map(|(binding, win)| json!({
                "binding": binding,
                "hwnd": win.hwnd,
                "deferred": Some(win.hwnd) == foreground_hwnd,
            })).collect::<Vec<_>>(),
            "restore": to_restore.iter().map(|(binding, win, _)| json!({
                "binding": binding, "hwnd": win.hwnd,
            })).collect::<Vec<_>>(),
            "raise_visible": to_bring_to_front.iter().map(|(win, _)| win.hwnd).collect::<Vec<_>>(),
            "warnings": warnings.iter().map(|warning| format!("{warning:?}")).collect::<Vec<_>>(),
        })
    });
    let trace_targets: Vec<_> = if trace.is_enabled() {
        to_restore
            .iter()
            .map(|(_, win, _)| win.hwnd)
            .chain(to_bring_to_front.iter().map(|(win, _)| win.hwnd))
            .collect()
    } else {
        Vec::new()
    };
    trace.stacking("before_operations", &trace_targets, sys);
    let mut deferred_foreground_minimize: Option<(String, WindowInstance)> = None;

    let mut minimized_hwnds = Vec::new();
    let mut verification_minimized = Vec::new();

    // Step 7a: Minimize all non-foreground windows in `to_minimize` using `SW_SHOWMINNOACTIVE`
    for (binding_id, win) in to_minimize {
        if foreground_hwnd == Some(win.hwnd) {
            deferred_foreground_minimize = Some((binding_id, win));
            continue;
        }
        if !can_still_minimize(sys, &win) {
            recovery.invalidate_hwnd(win.hwnd);
            continue;
        }
        match trace.show(sys, &binding_id, win.hwnd, ShowCmd::MinimizeNoActivate) {
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
    let mut scene_windows = Vec::new();

    let restore_cmd = if config.launcher.focus_on_switch && deferred_foreground_minimize.is_none() {
        ShowCmd::RestoreActivate
    } else {
        ShowCmd::RestoreNoActivate
    };
    for (binding_id, win, z) in to_restore {
        // Section 5.3: Always re-verify `is_iconic` immediately before sending restore command
        let live = sys.query_window(win.hwnd);
        let still_iconic = live.as_ref().is_some_and(|live| {
            live.is_iconic
                && stays_on_target(&win, live)
                && live.matches_runtime_identity(win.hwnd, win.pid, win.process_start_time)
        });
        if !still_iconic {
            trace.record("restore_skipped", || {
                json!({
                    "binding": binding_id,
                    "hwnd": win.hwnd,
                    "reason": "no_longer_same_minimized_window",
                    "observed": live.as_ref().map(window_state),
                })
            });
            recovery.invalidate_hwnd(win.hwnd);
            continue;
        }

        match trace.show(sys, &binding_id, win.hwnd, restore_cmd) {
            Ok(()) => {
                recovery.mark_restoring_by_us(win.hwnd);
                restored_hwnds.push(win.hwnd);
                verification_restored.push((binding_id, win.hwnd));
                scene_windows.push((win, z));
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
    let mut foreground_minimize_issued = false;
    if let Some((binding_id, fg_win)) = deferred_foreground_minimize {
        if !can_still_minimize(sys, &fg_win) {
            recovery.invalidate_hwnd(fg_win.hwnd);
        } else {
        match trace.show(sys, &binding_id, fg_win.hwnd, ShowCmd::MinimizeActivateNext) {
            Ok(()) => {
                recovery.mark_minimize_issued(fg_win.hwnd);
                minimized_hwnds.push(fg_win.hwnd);
                verification_minimized.push((binding_id, fg_win.hwnd));
                foreground_minimize_issued = true;
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
    }

    // Step 8b: Bring all next-scene windows (restored + already non-iconic) to the top of Z-order
    // in bottom-to-top order so they appear above previously open/unregistered apps,
    // and activate the topmost window so it doesn't get obscured behind active apps.
    scene_windows.append(&mut to_bring_to_front);
    if target_mon_id != "ALL_MONITORS" {
        scene_windows.retain(|(win, _)| {
            sys.query_window(win.hwnd).is_some_and(|live| {
                stays_on_target(win, &live)
                    && live.matches_runtime_identity(win.hwnd, win.pid, win.process_start_time)
            })
        });
    }
    scene_windows.sort_by_key(|(_, z)| std::cmp::Reverse(*z));
    let scene_windows: Vec<_> = scene_windows.iter().map(|(win, _)| win).collect();
    for win in scene_windows.iter().filter(|win| !win.is_hung) {
        trace.raise(sys, "initial", win.hwnd, WindowRaise::Normal);
    }
    let focus = config.launcher.focus_on_switch || foreground_minimize_issued;
    if focus {
        if let Some(hwnd) = scene_focus_target(&scene_windows) {
            trace.focus(sys, "initial", hwnd);
        }
    }
    trace.stacking("after_initial_requests", &trace_targets, sys);
    let presentation = input_tick
        .filter(|_| !scene_windows.is_empty())
        .map(|input_tick| PendingPresentation {
            windows: scene_windows
                .into_iter()
                .map(|win| BoundRuntimeIdentity {
                    hwnd: win.hwnd,
                    pid: win.pid,
                    process_start_time: win.process_start_time,
                })
                .collect(),
            focus,
            input_tick,
        });
    if let Some(presentation) = &presentation {
        repair_scene_presentation(
            presentation,
            &target_mon_id,
            sys,
            &trace,
            PresentationPhase::Immediate,
        );
    } else if !trace_targets.is_empty() {
        trace.record("presentation_skipped", || {
            json!({
                "phase": "immediate",
                "reason": if input_tick.is_none() { "input_tick_unavailable" } else { "no_eligible_targets" },
            })
        });
    }

    if !already_active || journal_dirty {
        let _ = recovery.persist_journal();
    }

    // Step 9: Confirm `state = Active(next)`
    state_mgr.set_active(next_scene_id, variant_id);
    if !warnings.is_empty() || trace.is_enabled() {
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
    }

    let verification = if !verification_minimized.is_empty()
        || !verification_restored.is_empty()
        || presentation.is_some()
        || !trace_targets.is_empty()
    {
        trace.record("verification_pending", || json!({}));
        trace.flush();
        Some(PendingVerification {
            scene_id: next_scene_id.to_string(),
            target_monitor_id: target_mon_id,
            minimized: verification_minimized,
            restored: verification_restored,
            presentation,
            trace,
            trace_targets,
        })
    } else {
        trace.record("finished", || json!({"reason": "no_window_operations"}));
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
/// hid themselves to the system tray, and checks Z-order and keyboard focus (FR-18).
pub fn verify_switch_results(
    pending: &PendingVerification,
    recovery: &mut RecoveryManager,
    sys: &mut dyn WindowSystem,
) -> Vec<SwitchWarning> {
    let mut warnings = Vec::new();
    let mut journal_dirty = false;
    let trace = &pending.trace;
    trace.stacking("verification_before", &pending.trace_targets, sys);

    for (binding_id, hwnd) in &pending.minimized {
        let live = sys.query_window(*hwnd);
        trace.record("show_verified", || {
            json!({
                "binding": binding_id, "hwnd": hwnd, "expected": "minimized",
                "observed": live.as_ref().map(window_state),
            })
        });
        if let Some(live) = live {
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
        let live = sys.query_window(*hwnd);
        trace.record("show_verified", || {
            json!({
                "binding": binding_id, "hwnd": hwnd, "expected": "restored",
                "observed": live.as_ref().map(window_state),
            })
        });
        if let Some(live) = live {
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

    if pending.presentation.is_none() {
        trace.record("presentation_skipped", || {
            json!({
                "phase": "verification", "reason": "no_pending_presentation",
            })
        });
    } else {
        warnings.extend(verify_scene_presentation(pending, sys));
    }

    // Section 5.4 (FR-18): Verify the current foreground window is not still a minimized window
    if let Some(fg_hwnd) = sys.get_foreground_window() {
        let fg_is_minimized = sys
            .query_window(fg_hwnd)
            .map(|w| w.is_iconic || !w.is_visible)
            .unwrap_or(false);
        if fg_is_minimized {
            let fallback_hwnd = pending
                .restored
                .iter()
                .rev()
                .find_map(|(_, hwnd)| sys.query_window(*hwnd).filter(can_receive_focus))
                .or_else(|| sys.enumerate_windows().into_iter().find(can_receive_focus))
                .map(|w| w.hwnd);
            if let Some(safe_hwnd) = fallback_hwnd {
                trace.focus(sys, "minimized_foreground_fallback", safe_hwnd);
            }
        }
    }

    if journal_dirty {
        let _ = recovery.persist_journal();
    }

    trace.stacking("verification_after", &pending.trace_targets, sys);
    trace.record("finished", || {
        json!({
            "reason": "verified",
            "warnings": warnings.iter().map(|warning| format!("{warning:?}")).collect::<Vec<_>>(),
        })
    });
    trace.flush();
    warnings
}

fn can_receive_focus(win: &WindowInstance) -> bool {
    win.is_visible && !win.is_iconic && win.is_enabled && !win.is_hung
}

fn needs_scene_presentation(
    windows: &[(WindowInstance, usize)],
    live_windows: &[WindowInstance],
    focus: bool,
    sys: &dyn WindowSystem,
) -> bool {
    if focus {
        let focus_target = windows
            .iter()
            .rev()
            .filter(|(win, _)| can_receive_focus(win))
            .min_by_key(|(_, z)| *z);
        if focus_target.is_some_and(|(win, _)| sys.get_foreground_window() != Some(win.hwnd)) {
            return true;
        }
    }

    let scene_hwnds: HashSet<_> = windows.iter().map(|(win, _)| win.hwnd).collect();
    windows
        .iter()
        .any(|(win, _)| !win.is_hung && is_covered_by_other_window(win, live_windows, &scene_hwnds))
}

fn is_covered_by_other_window(
    target: &WindowInstance,
    live_windows: &[WindowInstance],
    scene_hwnds: &HashSet<u64>,
) -> bool {
    live_windows
        .iter()
        .take_while(|above| above.hwnd != target.hwnd)
        .any(|above| {
            above.is_topmost == target.is_topmost
                && !above.is_iconic
                && !scene_hwnds.contains(&above.hwnd)
                && above.window_rect.intersection_area(&target.window_rect) > 0
        })
}

fn can_still_minimize(sys: &dyn WindowSystem, win: &WindowInstance) -> bool {
    sys.query_window(win.hwnd).is_some_and(|live| {
        live.matches_runtime_identity(win.hwnd, win.pid, win.process_start_time)
            && live.monitor_id == win.monitor_id
            && live.is_visible && !live.is_iconic && live.is_enabled && !live.is_elevated
    })
}

fn verify_scene_presentation(
    pending: &PendingVerification,
    sys: &mut dyn WindowSystem,
) -> Option<SwitchWarning> {
    let presentation = pending.presentation.as_ref()?;
    let (raise_failed, failed_focus_target) = repair_scene_presentation(
        presentation,
        &pending.target_monitor_id,
        sys,
        &pending.trace,
        PresentationPhase::Verification,
    );
    if raise_failed {
        log_warn(
            "switch",
            &format!(
                "Could not correct window stacking for scene '{}'",
                pending.scene_id
            ),
        );
    }
    if let Some(hwnd) = failed_focus_target {
        log_warn(
            "switch",
            &format!(
                "Could not focus window 0x{:X} in scene '{}'",
                hwnd, pending.scene_id
            ),
        );
    }
    if !raise_failed && failed_focus_target.is_none() {
        return None;
    }
    Some(SwitchWarning::ScenePresentationFailed {
        scene_id: pending.scene_id.clone(),
    })
}

/// Correct visual stacking, and optionally retry a foreground request after the switch settles.
fn repair_scene_presentation(
    presentation: &PendingPresentation,
    target_monitor_id: &str,
    sys: &mut dyn WindowSystem,
    trace: &SwitchTrace,
    phase: PresentationPhase,
) -> (bool, Option<u64>) {
    let retry_focus = phase.retries_focus() && presentation.focus;
    let current_input_tick = sys.last_input_tick();
    if current_input_tick != Some(presentation.input_tick) {
        trace.record("presentation_skipped", || {
            json!({
                "phase": phase, "reason": "input_changed_or_unavailable",
                "switch_input_tick": presentation.input_tick,
                "current_input_tick": current_input_tick,
            })
        });
        return (false, None);
    }

    // Revalidate candidates on the current desktop and keep their original Z-order.
    let live_windows = sys.enumerate_windows();
    let windows: Vec<_> = presentation
        .windows
        .iter()
        .filter_map(|target| {
            live_windows.iter().find(|live| {
                live.matches_runtime_identity(target.hwnd, target.pid, target.process_start_time)
                    && live.is_visible
                    && !live.is_iconic
                    && !live.is_hung
                    && (target_monitor_id == "ALL_MONITORS" || live.monitor_id == target_monitor_id)
            })
        })
        .collect();

    let scene_hwnds: HashSet<_> = windows.iter().map(|win| win.hwnd).collect();
    let is_covered = |target| is_covered_by_other_window(target, &live_windows, &scene_hwnds);
    let normal_windows_covered = windows.iter().any(|win| !win.is_topmost && is_covered(win));
    let topmost_windows_covered = windows.iter().any(|win| win.is_topmost && is_covered(win));
    trace.record("presentation_checked", || {
        json!({
            "phase": phase,
            "eligible_hwnds": windows.iter().map(|win| win.hwnd).collect::<Vec<_>>(),
            "normal_windows_covered": normal_windows_covered,
            "topmost_windows_covered": topmost_windows_covered,
            "retry_focus": retry_focus,
        })
    });

    let mut raise_failed = false;
    // Raise the affected group from bottom to top to preserve the scene's stacking order.
    for win in &windows {
        let covered = if win.is_topmost {
            topmost_windows_covered
        } else {
            normal_windows_covered
        };
        if covered {
            if sys.last_input_tick() != Some(presentation.input_tick) {
                trace.record("presentation_skipped", || {
                    json!({
                        "phase": phase, "reason": "input_changed_during_reorder",
                    })
                });
                return (raise_failed, None);
            }
            raise_failed |= !trace.raise(
                sys,
                phase.as_str(),
                win.hwnd,
                WindowRaise::ReorderWithinGroup,
            );
        }
    }
    let failed_focus_target = if retry_focus {
        let current_input_tick = sys.last_input_tick();
        if current_input_tick == Some(presentation.input_tick) {
            scene_focus_target(&windows).filter(|&hwnd| {
                sys.get_foreground_window() != Some(hwnd)
                    && !trace.focus(sys, phase.as_str(), hwnd).accepted()
            })
        } else {
            trace.record("focus_skipped", || {
                json!({
                    "phase": phase, "reason": "input_changed_or_unavailable",
                    "switch_input_tick": presentation.input_tick,
                    "current_input_tick": current_input_tick,
                })
            });
            None
        }
    } else {
        None
    };
    (raise_failed, failed_focus_target)
}

fn binding_display_name(
    binding_id: &str,
    bindings: &[crate::config::WindowBinding],
) -> String {
    if let Some(b) = bindings.iter().find(|b| b.id == binding_id) {
        let exe = std::path::Path::new(&b.match_rule.exe_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&b.match_rule.exe_path);
        if exe.is_empty() {
            binding_id.to_string()
        } else {
            format!("{} ({})", exe, binding_id)
        }
    } else {
        binding_id.to_string()
    }
}

/// Formats `SwitchWarning` entries into user-readable Japanese explanation lines.
pub fn format_switch_warnings(
    warnings: &[SwitchWarning],
    bindings: &[crate::config::WindowBinding],
    scenes: &[crate::config::Scene],
) -> Vec<String> {
    warnings
        .iter()
        .map(|w| match w {
            SwitchWarning::TargetMonitorNotConfigured => {
                "切り替える画面を設定してください。「シーン」または「ウィンドウの動作」から選べます。".to_string()
            }
            SwitchWarning::TargetMonitorDisconnected { monitor_id } => {
                format!(
                    "設定された対象モニタ ({}) が現在接続されていません。",
                    monitor_id
                )
            }
            SwitchWarning::SceneNotFound { scene_id } => {
                format!("指定されたシーン ID ({}) が見つかりませんでした。", scene_id)
            }
            SwitchWarning::SceneVariantNotFound { scene_id } => {
                let name = scenes.iter().find(|scene| &scene.id == scene_id)
                    .map_or(scene_id.as_str(), |scene| scene.name.as_str());
                format!("「{name}」で選んだ構成が見つかりませんでした。")
            }
            SwitchWarning::AmbiguousBindingSameMonitor {
                binding_id,
                candidate_hwnds,
            } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 同一モニタ上に一致するウィンドウが {} 個あり、特定できないため安全のためスキップしました。設定の「アプリ一覧」タブで候補を特定してください。",
                    app,
                    candidate_hwnds.len()
                )
            }
            SwitchWarning::AmbiguousBindingCrossMonitor {
                binding_id,
                target_hwnd,
            } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 別モニタにも同名ウィンドウが存在します（対象モニタ上の HWND 0x{:X} を優先しました）。",
                    app, target_hwnd
                )
            }
            SwitchWarning::SkippedModalDisabledWindow { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] モーダルダイアログ等が表示中のため、最小化をスキップしました。",
                    app
                )
            }
            SwitchWarning::SkippedElevatedWindow { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 管理者権限プロセスまたはアクセス拒否のため、操作をスキップしました。",
                    app
                )
            }
            SwitchWarning::HungWindowDetected { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] ウィンドウが応答なし状態のため、操作をスキップしました。",
                    app
                )
            }
            SwitchWarning::HiddenToTrayOnMinimize { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 最小化時にシステムトレイ等へ格納されたため、Kiri からの自動復元対象外となりました。",
                    app
                )
            }
            SwitchWarning::MinimizeVerificationFailed { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 最小化要求後の状態確認で最小化を確認できませんでした。",
                    app
                )
            }
            SwitchWarning::RestoreVerificationFailed { binding_id, .. } => {
                let app = binding_display_name(binding_id, bindings);
                format!(
                    "[{}] 復元要求後の状態確認で表示を確認できませんでした。",
                    app
                )
            }
            SwitchWarning::ScenePresentationFailed { scene_id } => {
                let scene_name = scenes
                    .iter()
                    .find(|s| s.id == *scene_id)
                    .map(|s| s.name.as_str())
                    .unwrap_or(scene_id.as_str());
                format!(
                    "シーン「{}」の重なり順またはフォーカス更新が Windows に拒否されました。",
                    scene_name
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod format_warning_tests {
    use super::*;
    use crate::config::{Scene, TitleRule, WindowBinding, WindowMatch};

    #[test]
    fn formats_warnings_with_app_and_scene_names() {
        let bindings = vec![WindowBinding {
            id: "b1".to_string(),
            match_rule: WindowMatch {
                exe_path: r"C:\Apps\Code.exe".to_string(),
                window_class: String::new(),
                title_rule: TitleRule::any(),
                aumid: None,
                monitor_id: None,
            },
            instance_hint: None,
            scene_ids: vec!["code".to_string()],
            scene_variants: Default::default(),
            always_visible: false,
            enabled: true,
        }];
        let scenes = vec![Scene {
            id: "code".to_string(),
            name: "コード".to_string(),
            icon: "code".to_string(),
            pinned: true,
            order: 0,
            ..Scene::default()
        }];
        let warnings = vec![
            SwitchWarning::AmbiguousBindingSameMonitor {
                binding_id: "b1".to_string(),
                candidate_hwnds: vec![1, 2],
            },
            SwitchWarning::ScenePresentationFailed {
                scene_id: "code".to_string(),
            },
        ];
        let lines = format_switch_warnings(&warnings, &bindings, &scenes);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Code.exe"));
        assert!(lines[0].contains("2 個"));
        assert!(lines[1].contains("コード"));
    }
}
