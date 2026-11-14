//! Central application state and coordinator (`AppController`).
//!
//! Coordinates `ConfigManager`, `SceneStateManager`, `BindingResolver`, `RecoveryManager`,
//! and `WindowSystem` without coupling the core logic to Win32 `HWND` callbacks.

use crate::config::{AppConfig, ConfigManager, MonitorProfile, Orientation};
use crate::diagnostics::{log_info, log_warn};
use crate::launcher::input::LauncherLayout;
use crate::platform::hooks::drain_minimize_events;
use crate::platform::monitors::{compute_saved_position_with_dpi, resolve_launcher_position};
use crate::platform::WindowSystem;
use crate::recovery::{ManagedAction, RecoveryManager};
use crate::scenes::bindings::BindingResolver;
use crate::scenes::layouts::{
    apply_saved_scene_layout, capture_scene_layout, save_layout_into_config,
};
use crate::scenes::manager::SceneStateManager;
use crate::scenes::switch::{
    execute_scene_switch_from, resolve_effective_target_monitor_id, verify_switch_results,
    PendingVerification, SwitchOutcome, SwitchSource, SwitchWarning,
};

pub struct AppController {
    pub config_mgr: ConfigManager,
    pub config: AppConfig,
    pub state_mgr: SceneStateManager,
    pub resolver: BindingResolver,
    pub recovery: RecoveryManager,
    pub sys: Box<dyn WindowSystem>,
    pub pending_verification: Option<PendingVerification>,
    pub last_warnings: Vec<SwitchWarning>,
    pub launcher_monitor_id: String,
    pub launcher_dpi: u32,
}

impl AppController {
    pub fn new(
        mut config_mgr: ConfigManager,
        recovery: RecoveryManager,
        sys: Box<dyn WindowSystem>,
    ) -> Self {
        let config = config_mgr.load();
        crate::diagnostics::set_verbose_title_logging(config.verbose_title_logging);
        let last_scene = config
            .target
            .monitor_id
            .as_ref()
            .and_then(|target_id| {
                config
                    .monitors
                    .iter()
                    .find(|m| &m.monitor_id == target_id)
                    .and_then(|m| m.last_active_scene_id.clone())
            });

        Self {
            config_mgr,
            config,
            state_mgr: SceneStateManager::new(last_scene),
            resolver: BindingResolver::new(),
            recovery,
            sys,
            pending_verification: None,
            last_warnings: Vec::new(),
            launcher_monitor_id: String::new(),
            launcher_dpi: 96,
        }
    }

    /// Inspect crash journal from any prior abnormal exit (Section 8.1 & AT-27).
    /// Returns the list of surviving windows minimized by the previous session.
    pub fn check_startup_crash_recovery(&mut self) -> Vec<ManagedAction> {
        self.state_mgr.reset_to_unknown();
        self.recovery.check_previous_crash_journal(&*self.sys)
    }

    /// Commit new settings before changing the in-memory configuration or launcher behavior.
    pub fn save_user_settings(&mut self, config: AppConfig) -> Result<(), String> {
        self.config_mgr.save(&config)?;
        for binding in &self.config.bindings {
            if !config.bindings.iter().any(|next| next.id == binding.id
                && next.match_rule == binding.match_rule
                && next.instance_hint == binding.instance_hint && next.enabled == binding.enabled)
            {
                self.resolver.clear_binding(&binding.id);
            }
        }
        if let Some(pending) = &mut self.pending_verification {
            pending.cancel_presentation_retry();
        }
        if self.config.target != config.target || self.state_mgr.active_scene_id().is_some_and(|id| {
            !config.scenes.iter().any(|s| s.id == id)
        }) {
            self.state_mgr.reset_to_unknown();
        }
        crate::diagnostics::set_verbose_title_logging(config.verbose_title_logging);
        self.config = config;
        Ok(())
    }

    /// Compute the current `LauncherLayout` for `dpi`.
    pub fn compute_layout(&self, dpi: u32) -> LauncherLayout {
        let (visible, overflow) = self.config.split_visible_and_overflow();
        LauncherLayout::compute(&self.config.launcher, &visible, !overflow.is_empty(), dpi)
    }

    /// Resolve the launcher's screen `(x, y, monitor_id, dpi)` based on saved position and connected monitors.
    pub fn resolve_initial_or_updated_launcher_rect(&mut self) -> (i32, i32, i32, i32) {
        let monitors = self.sys.monitors();
        let (visible, overflow) = self.config.split_visible_and_overflow();
        let has_overflow = !overflow.is_empty();
        let launcher_cfg = self.config.launcher.clone();

        let (x, y, mon_id, dpi) = resolve_launcher_position(
            self.config.launcher.position.as_ref(),
            &monitors,
            |d| {
                let l = LauncherLayout::compute(&launcher_cfg, &visible, has_overflow, d);
                (l.width_px, l.height_px)
            },
            self.config.launcher.snap_to_edges,
        );

        self.launcher_monitor_id = mon_id;
        self.launcher_dpi = dpi;
        let layout = self.compute_layout(dpi);
        (x, y, layout.width_px, layout.height_px)
    }

    /// Persist launcher position when drag finishes (Section 2.2).
    pub fn on_launcher_drag_end(&mut self, screen_x: i32, screen_y: i32) -> (i32, i32, i32, i32) {
        self.on_launcher_drag_end_at(screen_x, screen_y, None)
    }

    /// Persist launcher position when drag finishes, optionally attributing the target monitor
    /// using the mouse cursor / grip drop coordinate `cursor_anchor`.
    pub fn on_launcher_drag_end_at(
        &mut self,
        screen_x: i32,
        screen_y: i32,
        cursor_anchor: Option<(i32, i32)>,
    ) -> (i32, i32, i32, i32) {
        let monitors = self.sys.monitors();
        let cur_layout = self.compute_layout(self.launcher_dpi);
        let (visible, overflow) = self.config.split_visible_and_overflow();
        let has_overflow = !overflow.is_empty();
        let launcher_cfg = self.config.launcher.clone();

        if let Some((clamped_x, clamped_y, w_px, h_px, saved)) = compute_saved_position_with_dpi(
            screen_x,
            screen_y,
            cur_layout.width_px,
            cur_layout.height_px,
            cursor_anchor,
            &monitors,
            |d| {
                let l = LauncherLayout::compute(&launcher_cfg, &visible, has_overflow, d);
                (l.width_px, l.height_px)
            },
            self.config.launcher.snap_to_edges,
        ) {
            self.launcher_monitor_id = saved.monitor_id.clone();
            self.launcher_dpi = saved.dpi;
            self.config.launcher.position = Some(saved);
            let _ = self.config_mgr.save(&self.config);
            return (clamped_x, clamped_y, w_px, h_px);
        }

        self.resolve_initial_or_updated_launcher_rect()
    }

    /// Reset launcher position to default on its current or primary monitor (Section 2.2).
    pub fn reset_launcher_position(&mut self) -> (i32, i32, i32, i32) {
        self.config.launcher.position = None;
        let _ = self.config_mgr.save(&self.config);
        self.resolve_initial_or_updated_launcher_rect()
    }

    /// Toggle Horizontal <-> Vertical orientation and persist (FR-03, AT-05).
    pub fn toggle_orientation(&mut self) -> (i32, i32, i32, i32) {
        self.config.launcher.orientation = match self.config.launcher.orientation {
            Orientation::Horizontal => Orientation::Vertical,
            Orientation::Vertical => Orientation::Horizontal,
        };
        let _ = self.config_mgr.save(&self.config);
        self.resolve_initial_or_updated_launcher_rect()
    }

    /// Drain and process any queued `EVENT_SYSTEM_MINIMIZESTART` / `MINIMIZEEND` events.
    pub fn process_queued_win_events(&mut self) {
        let events = drain_minimize_events();
        if !events.is_empty() {
            self.recovery.process_minimize_events(&events);
        }
    }

    /// Request switching to `scene_id` (serialized on main UI thread).
    pub fn switch_to_scene(&mut self, scene_id: &str, source: SwitchSource) -> SwitchOutcome {
        self.process_queued_win_events();

        let launcher_mon = if self.launcher_monitor_id.is_empty() {
            None
        } else {
            Some(self.launcher_monitor_id.as_str())
        };

        let outcome = execute_scene_switch_from(
            scene_id,
            &self.config,
            &mut self.state_mgr,
            &mut self.resolver,
            &mut self.recovery,
            &mut *self.sys,
            launcher_mon,
            source,
        );

        if outcome.switched {
            if let Some(previous) = &self.pending_verification {
                previous.skip_verification("superseded_by_next_switch");
            }
            self.pending_verification = outcome.verification.clone();
            self.last_warnings = outcome.warnings.clone();
            self.update_monitor_profile_active_scene(scene_id);
        } else if !outcome.was_noop_same_scene {
            self.last_warnings = outcome.warnings.clone();
        }

        outcome
    }

    /// Called when the one-shot post-switch verification timer fires (~500ms after switch, Step 10).
    pub fn run_post_switch_verification(&mut self) -> Vec<SwitchWarning> {
        self.process_queued_win_events();
        let Some(pending) = self.pending_verification.take() else {
            return Vec::new();
        };
        let extra_warnings = verify_switch_results(&pending, &mut self.recovery, &mut *self.sys);
        self.last_warnings.extend(extra_warnings.clone());
        extra_warnings
    }

    /// Restore all windows minimized by Kiri and reset scene state to `Unknown` (Section 8.1, AT-15).
    pub fn restore_all_minimized_windows(&mut self) -> usize {
        self.restore_minimized_windows(None)
    }

    pub fn restore_settings_windows(&mut self, windows: &[crate::platform::WindowInstance]) -> usize {
        self.restore_minimized_windows(Some(windows))
    }

    fn restore_minimized_windows(&mut self, windows: Option<&[crate::platform::WindowInstance]>) -> usize {
        self.process_queued_win_events();
        if let Some(pending) = self.pending_verification.take() {
            pending.skip_verification("restore_all");
        }
        let count = self.recovery.restore_minimized_by_us(&mut *self.sys, windows);
        self.state_mgr.reset_to_unknown();
        self.last_warnings.clear();
        count
    }

    /// Save the layout of the currently active scene (Section 5.2, FR-12).
    pub fn save_active_scene_layout(&mut self) -> bool {
        let Some(scene_id) = self.state_mgr.active_scene_id().map(|s| s.to_string()) else {
            log_warn("layout", "Cannot save layout when scene state is Unknown");
            return false;
        };

        let target_monitors: Vec<String> =
            if self.config.target.monitor_policy == crate::config::MonitorPolicy::AllMonitors {
                self.sys
                    .monitors()
                    .into_iter()
                    .map(|m| m.monitor_id)
                    .collect()
            } else {
                let launcher_mon = if self.launcher_monitor_id.is_empty() {
                    None
                } else {
                    Some(self.launcher_monitor_id.as_str())
                };
                match resolve_effective_target_monitor_id(&self.config, launcher_mon) {
                    Some(mon) => vec![mon],
                    None => return false,
                }
            };

        let mut any_saved = false;
        for target_mon in target_monitors {
            if let Some(snapshot) = capture_scene_layout(
                &scene_id,
                &target_mon,
                &self.config,
                &mut self.resolver,
                &*self.sys,
            ) {
                save_layout_into_config(&mut self.config, snapshot);
                any_saved = true;
            }
        }

        if any_saved {
            let _ = self.config_mgr.save(&self.config);
            log_info(
                "layout",
                &format!("Saved layout snapshot for scene '{}'", scene_id),
            );
            true
        } else {
            false
        }
    }

    /// Restore the saved layout of the currently active scene (Section 5.2, FR-12).
    pub fn restore_active_scene_layout(&mut self) -> usize {
        let Some(scene_id) = self.state_mgr.active_scene_id().map(|s| s.to_string()) else {
            return 0;
        };

        let target_monitors: Vec<String> =
            if self.config.target.monitor_policy == crate::config::MonitorPolicy::AllMonitors {
                self.sys
                    .monitors()
                    .into_iter()
                    .map(|m| m.monitor_id)
                    .collect()
            } else {
                let launcher_mon = if self.launcher_monitor_id.is_empty() {
                    None
                } else {
                    Some(self.launcher_monitor_id.as_str())
                };
                match resolve_effective_target_monitor_id(&self.config, launcher_mon) {
                    Some(mon) => vec![mon],
                    None => return 0,
                }
            };

        let mut total = 0;
        for target_mon in target_monitors {
            total += apply_saved_scene_layout(
                &scene_id,
                &target_mon,
                &self.config,
                &mut self.resolver,
                &mut *self.sys,
            );
        }
        total
    }

    /// Explicitly launches unregistered/not-running executable apps belonging to the active scene.
    /// NEVER called during automatic scene switching (only via explicit user menu action).
    /// Returns `(launched_count, skipped_count, error_messages)`.
    pub fn launch_missing_apps_for_active_scene(&mut self) -> (usize, usize, Vec<String>) {
        let Some(scene_id) = self.state_mgr.active_scene_id().map(|s| s.to_string()) else {
            return (0, 0, Vec::new());
        };
        let windows = self.sys.enumerate_windows();
        let monitors = self.sys.monitors();
        let target_monitors: Vec<String> =
            if self.config.target.monitor_policy == crate::config::MonitorPolicy::AllMonitors {
                monitors.iter().map(|m| m.monitor_id.clone()).collect()
            } else {
                let launcher_mon = if self.launcher_monitor_id.is_empty() {
                    None
                } else {
                    Some(self.launcher_monitor_id.as_str())
                };
                match resolve_effective_target_monitor_id(&self.config, launcher_mon) {
                    Some(mon) => vec![mon],
                    None => Vec::new(),
                }
            };

        let mut launched = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();
        let mut seen_exes = std::collections::HashSet::new();

        for binding in &self.config.bindings {
            if !binding.enabled || !binding.scene_ids.iter().any(|s| s == &scene_id) {
                continue;
            }
            let already_open = target_monitors.iter().any(|mon| {
                !matches!(
                    self.resolver.resolve_binding(binding, &windows, mon),
                    crate::scenes::bindings::BindingResolution::NotFound
                )
            });
            if already_open {
                continue;
            }
            let exe = binding.match_rule.exe_path.trim();
            if exe.is_empty() {
                skipped += 1;
                continue;
            }
            let exe_lower = exe.to_ascii_lowercase();
            // UWP / ApplicationFrameHost cannot be meaningfully re-launched directly via exe path
            if exe_lower.ends_with("\\applicationframehost.exe") {
                skipped += 1;
                continue;
            }
            if !seen_exes.insert(exe_lower) {
                continue;
            }
            if !std::path::Path::new(exe).is_file() {
                skipped += 1;
                errors.push(format!("実行ファイルが見つかりません: {}", exe));
                continue;
            }
            match std::process::Command::new(exe).spawn() {
                Ok(_) => {
                    launched += 1;
                    log_info(
                        "app",
                        &format!(
                            "Explicitly launched missing app for scene '{}': {}",
                            scene_id, exe
                        ),
                    );
                }
                Err(e) => {
                    skipped += 1;
                    errors.push(format!("{}: {}", exe, e));
                }
            }
        }

        (launched, skipped, errors)
    }

    /// Returns true if the currently active scene is in the `⋯` overflow list (Section 2.3).
    pub fn is_active_scene_in_overflow(&self) -> bool {
        let Some(active_id) = self.state_mgr.active_scene_id() else {
            return false;
        };
        let (_, overflow) = self.config.split_visible_and_overflow();
        overflow.iter().any(|s| s.id == active_id)
    }

    /// Normal application shutdown: restore all windows minimized by Kiri and clear journal (Section 8.1, AT-16).
    pub fn on_normal_shutdown(&mut self) {
        let restored = self.recovery.restore_all_minimized_by_us(&mut *self.sys);
        let _ = self.recovery.clear_journal();
        log_info(
            "app",
            &format!(
                "Normal shutdown completed; restored {} window(s) and cleared journal",
                restored
            ),
        );
    }

    /// OS logoff/shutdown (`WM_ENDSESSION`): do NOT restore windows; flush journal and exit quickly (Section 7.2 & 8.2).
    pub fn on_os_session_end(&mut self) {
        let _ = self.recovery.persist_journal();
        log_info(
            "app",
            "OS session ending (WM_ENDSESSION); flushed runtime journal without restoring windows",
        );
    }

    fn update_monitor_profile_active_scene(&mut self, scene_id: &str) {
        let launcher_mon = if self.launcher_monitor_id.is_empty() {
            None
        } else {
            Some(self.launcher_monitor_id.as_str())
        };
        let Some(target_mon) = resolve_effective_target_monitor_id(&self.config, launcher_mon)
        else {
            return;
        };

        if let Some(prof) = self
            .config
            .monitors
            .iter_mut()
            .find(|m| m.monitor_id == target_mon)
        {
            prof.last_active_scene_id = Some(scene_id.to_string());
        } else {
            self.config.monitors.push(MonitorProfile {
                monitor_id: target_mon,
                friendly_name: None,
                work_area: Default::default(),
                dpi: self.launcher_dpi,
                last_active_scene_id: Some(scene_id.to_string()),
            });
        }
    }
}
