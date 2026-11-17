//! Central application state and coordinator (`AppController`).
//!
//! Coordinates `ConfigManager`, `SceneStateManager`, `BindingResolver`, `RecoveryManager`,
//! and `WindowSystem` without coupling the core logic to Win32 `HWND` callbacks.

use crate::config::{
    AppConfig, BarDisplayMode, ConfigManager, MonitorPolicy, MonitorProfile, Orientation,
    TargetConfig,
};
use crate::diagnostics::{log_info, log_warn};
use crate::launcher::input::LauncherLayout;
use crate::platform::hooks::drain_minimize_events;
use crate::platform::monitors::{
    compute_saved_position_on_monitor, compute_saved_position_with_dpi,
    default_launcher_position_on_monitor, resolve_launcher_bars, resolve_launcher_position,
    ResolvedLauncherBar,
};
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
        if self.state_mgr.active_scene_id().is_some_and(|id| {
            let previous = self
                .config
                .scene(id)
                .map(|scene| scene.target(&self.config.target));
            config.scene(id).is_none_or(|scene| {
                previous != Some(scene.target(&config.target))
                    || self.state_mgr.active_variant_id()
                        != scene.preferred_variant().map(|variant| variant.id.as_str())
            })
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

    /// Resolve all active launcher bar rectangles according to `bar_display_mode`.
    pub fn resolve_launcher_bars(&mut self) -> Vec<ResolvedLauncherBar> {
        let monitors = self.sys.monitors();
        let (visible, overflow) = self.config.split_visible_and_overflow();
        let has_overflow = !overflow.is_empty();
        let launcher_cfg = self.config.launcher.clone();

        let bars = resolve_launcher_bars(&launcher_cfg, &monitors, |d| {
            let l = LauncherLayout::compute(&launcher_cfg, &visible, has_overflow, d);
            (l.width_px, l.height_px)
        });

        if let Some(current) = bars
            .iter()
            .find(|bar| bar.monitor_id == self.launcher_monitor_id)
            .or_else(|| bars.first())
        {
            self.launcher_monitor_id = current.monitor_id.clone();
            self.launcher_dpi = current.dpi;
        } else {
            let (_, _, mon_id, dpi) = resolve_launcher_position(
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
        }

        bars
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
            self.config
                .launcher
                .positions_by_monitor
                .insert(saved.monitor_id.clone(), saved.clone());
            self.config.launcher.position = Some(saved);
            let _ = self.config_mgr.save(&self.config);
            return (clamped_x, clamped_y, w_px, h_px);
        }

        self.resolve_initial_or_updated_launcher_rect()
    }

    /// Persist a specific bar's position when drag finishes.
    /// In `Single` mode, allows dragging across monitors; in multi-bar modes, keeps the bar on `bar_monitor_id`.
    pub fn on_bar_drag_end(
        &mut self,
        bar_monitor_id: &str,
        screen_x: i32,
        screen_y: i32,
        bar_dpi: u32,
        cursor_anchor: Option<(i32, i32)>,
    ) -> Option<ResolvedLauncherBar> {
        let monitors = self.sys.monitors();
        let cur_layout = self.compute_layout(bar_dpi);
        let (visible, overflow) = self.config.split_visible_and_overflow();
        let has_overflow = !overflow.is_empty();
        let launcher_cfg = self.config.launcher.clone();
        let size_fn = |d| {
            let l = LauncherLayout::compute(&launcher_cfg, &visible, has_overflow, d);
            (l.width_px, l.height_px)
        };

        let result = if self.config.launcher.bar_display_mode == BarDisplayMode::Single {
            compute_saved_position_with_dpi(
                screen_x,
                screen_y,
                cur_layout.width_px,
                cur_layout.height_px,
                cursor_anchor,
                &monitors,
                size_fn,
                self.config.launcher.snap_to_edges,
            )
        } else {
            let mon = monitors
                .iter()
                .find(|m| m.monitor_id == bar_monitor_id)
                .or_else(|| monitors.first())?;
            Some(compute_saved_position_on_monitor(
                screen_x,
                screen_y,
                mon,
                &size_fn,
                self.config.launcher.snap_to_edges,
            ))
        };

        if let Some((clamped_x, clamped_y, w_px, h_px, saved)) = result {
            self.launcher_monitor_id = saved.monitor_id.clone();
            self.launcher_dpi = saved.dpi;
            self.config
                .launcher
                .positions_by_monitor
                .insert(saved.monitor_id.clone(), saved.clone());
            if self.config.launcher.bar_display_mode == BarDisplayMode::Single
                || self
                    .config
                    .launcher
                    .position
                    .as_ref()
                    .is_none_or(|p| p.monitor_id == saved.monitor_id)
            {
                self.config.launcher.position = Some(saved.clone());
            }
            let _ = self.config_mgr.save(&self.config);
            return Some(ResolvedLauncherBar {
                monitor_id: saved.monitor_id,
                x: clamped_x,
                y: clamped_y,
                width_px: w_px,
                height_px: h_px,
                dpi: saved.dpi,
            });
        }

        None
    }

    /// Reset launcher position to default on its current or primary monitor (Section 2.2).
    pub fn reset_launcher_position(&mut self) -> (i32, i32, i32, i32) {
        self.config.launcher.position = None;
        self.config.launcher.positions_by_monitor.clear();
        let _ = self.config_mgr.save(&self.config);
        self.resolve_initial_or_updated_launcher_rect()
    }

    /// Reset a specific bar's position (or all bars in `Single` mode / when `monitor_id` is `None`).
    pub fn reset_bar_position(&mut self, monitor_id: Option<&str>) {
        if self.config.launcher.bar_display_mode == BarDisplayMode::Single || monitor_id.is_none() {
            self.config.launcher.position = None;
            self.config.launcher.positions_by_monitor.clear();
            let _ = self.config_mgr.save(&self.config);
            return;
        }
        let Some(mon_id) = monitor_id else {
            return;
        };
        let monitors = self.sys.monitors();
        let Some(mon) = monitors.iter().find(|m| m.monitor_id == mon_id) else {
            self.config.launcher.positions_by_monitor.remove(mon_id);
            let _ = self.config_mgr.save(&self.config);
            return;
        };
        let (visible, overflow) = self.config.split_visible_and_overflow();
        let has_overflow = !overflow.is_empty();
        let launcher_cfg = self.config.launcher.clone();
        let size_fn = |d| {
            let l = LauncherLayout::compute(&launcher_cfg, &visible, has_overflow, d);
            (l.width_px, l.height_px)
        };
        let (w_px, h_px) = size_fn(mon.dpi);
        let (def_x, def_y) =
            default_launcher_position_on_monitor(mon.work_area, mon.dpi, w_px, h_px);
        let (_, _, _, _, saved) = compute_saved_position_on_monitor(
            def_x,
            def_y,
            mon,
            &size_fn,
            self.config.launcher.snap_to_edges,
        );
        self.config
            .launcher
            .positions_by_monitor
            .insert(mon_id.to_owned(), saved.clone());
        if self
            .config
            .launcher
            .position
            .as_ref()
            .is_some_and(|p| p.monitor_id == mon_id)
        {
            self.config.launcher.position = Some(saved);
        }
        let _ = self.config_mgr.save(&self.config);
    }

    /// Hide a bar from `monitor_id` when multiple bars are displayed. Returns `true` if updated.
    pub fn hide_bar_on_monitor(&mut self, monitor_id: &str) -> bool {
        if !self.config.launcher.show_bar {
            return false;
        }
        let monitors = self.sys.monitors();
        match self.config.launcher.bar_display_mode {
            BarDisplayMode::Single => return false,
            BarDisplayMode::AllMonitors => {
                let remaining: Vec<String> = monitors
                    .into_iter()
                    .map(|m| m.monitor_id)
                    .filter(|id| id != monitor_id)
                    .collect();
                if remaining.is_empty() {
                    return false;
                }
                self.config.launcher.bar_display_mode = BarDisplayMode::SelectedMonitors;
                self.config.launcher.bar_monitor_ids = remaining;
            }
            BarDisplayMode::SelectedMonitors => {
                let connected_remaining = monitors
                    .iter()
                    .filter(|m| {
                        m.monitor_id != monitor_id
                            && self
                                .config
                                .launcher
                                .bar_monitor_ids
                                .iter()
                                .any(|id| id == &m.monitor_id)
                    })
                    .count();
                if connected_remaining == 0 {
                    return false;
                }
                self.config
                    .launcher
                    .bar_monitor_ids
                    .retain(|id| id != monitor_id);
            }
        }
        let _ = self.config_mgr.save(&self.config);
        true
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

    /// Toggle Horizontal <-> Vertical orientation and persist, without assuming a single bar.
    pub fn toggle_orientation_and_save(&mut self) {
        self.config.launcher.orientation = match self.config.launcher.orientation {
            Orientation::Horizontal => Orientation::Vertical,
            Orientation::Vertical => Orientation::Horizontal,
        };
        let _ = self.config_mgr.save(&self.config);
    }

    /// Drain and process any queued `EVENT_SYSTEM_MINIMIZESTART` / `MINIMIZEEND` events.
    pub fn process_queued_win_events(&mut self) {
        let events = drain_minimize_events();
        if !events.is_empty() {
            self.recovery.process_minimize_events(&events);
        }
    }

    /// Switch to the requested variant, or the scene's last-used variant for an ordinary click.
    pub fn switch_to_scene(
        &mut self,
        scene_id: &str,
        variant_id: Option<&str>,
        source: SwitchSource,
    ) -> SwitchOutcome {
        self.switch_to_scene_from_monitor(scene_id, variant_id, source, None)
    }

    /// Switch to the requested scene/variant, attributing `MonitorPolicy::LauncherMonitor`
    /// to `source_monitor_id` when triggered from a specific launcher bar.
    pub fn switch_to_scene_from_monitor(
        &mut self,
        scene_id: &str,
        variant_id: Option<&str>,
        source: SwitchSource,
        source_monitor_id: Option<&str>,
    ) -> SwitchOutcome {
        self.process_queued_win_events();

        if let Some(mon_id) = source_monitor_id {
            if !mon_id.is_empty() {
                self.launcher_monitor_id = mon_id.to_owned();
                if let Some(mon) = self
                    .sys
                    .monitors()
                    .into_iter()
                    .find(|m| m.monitor_id == mon_id)
                {
                    self.launcher_dpi = mon.dpi;
                }
            }
        }

        let launcher_mon = source_monitor_id
            .filter(|id| !id.is_empty())
            .or_else(|| {
                (!self.launcher_monitor_id.is_empty()).then_some(self.launcher_monitor_id.as_str())
            });

        let outcome = execute_scene_switch_from(
            scene_id,
            variant_id,
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
            if let Some(variant_id) = self.state_mgr.active_variant_id() {
                if let Some(scene) = self
                    .config
                    .scenes
                    .iter_mut()
                    .find(|scene| scene.id == scene_id)
                {
                    if scene.last_variant_id.as_deref() != Some(variant_id) {
                        scene.last_variant_id = Some(variant_id.to_owned());
                        if let Err(error) = self.config_mgr.save(&self.config) {
                            log_warn(
                                "config",
                                &format!("Could not remember scene variant: {error}"),
                            );
                        }
                    }
                }
            }
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

        let variant_id = self.state_mgr.active_variant_id();
        let mut any_saved = false;
        for target_mon in self.scene_monitor_ids(&scene_id) {
            if let Some(snapshot) = capture_scene_layout(
                &scene_id,
                variant_id,
                &target_mon,
                &self.config,
                &mut self.resolver,
                &*self.sys,
            ) {
                save_layout_into_config(&mut self.config, snapshot, variant_id);
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

        let mut total = 0;
        for target_mon in self.scene_monitor_ids(&scene_id) {
            total += apply_saved_scene_layout(
                &scene_id,
                self.state_mgr.active_variant_id(),
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
        let target_monitors = self.scene_monitor_ids(&scene_id);
        if target_monitors.is_empty() {
            return (0, 0, Vec::new());
        }
        let windows = self.sys.enumerate_windows();
        let target_is_fixed =
            self.scene_target(&scene_id).monitor_policy != MonitorPolicy::AllMonitors;
        let variant_id = self.state_mgr.active_variant_id();

        let mut launched = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();
        let mut seen_exes = std::collections::HashSet::new();

        for binding in &self.config.bindings {
            if !binding.enabled || !binding.belongs_to_selection(&scene_id, variant_id) {
                continue;
            }
            if target_is_fixed
                && binding
                    .match_rule
                    .monitor_id
                    .as_ref()
                    .is_some_and(|id| !target_monitors.contains(id))
            {
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

    /// Normal application shutdown: clear journal without restoring minimized windows.
    pub fn on_normal_shutdown(&mut self) {
        let _ = self.recovery.clear_journal();
        log_info(
            "app",
            "Normal shutdown completed; cleared journal without restoring windows",
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

    fn scene_target(&self, scene_id: &str) -> &TargetConfig {
        self.config
            .scene(scene_id)
            .map_or(&self.config.target, |scene| {
                scene.target(&self.config.target)
            })
    }

    fn launcher_monitor(&self) -> Option<&str> {
        (!self.launcher_monitor_id.is_empty()).then_some(self.launcher_monitor_id.as_str())
    }

    fn scene_monitor_ids(&self, scene_id: &str) -> Vec<String> {
        let target = self.scene_target(scene_id);
        let monitors = self.sys.monitors();
        if target.monitor_policy == MonitorPolicy::AllMonitors {
            return monitors.into_iter().map(|mon| mon.monitor_id).collect();
        }
        resolve_effective_target_monitor_id(target, self.launcher_monitor())
            .filter(|id| monitors.iter().any(|mon| &mon.monitor_id == id))
            .into_iter()
            .collect()
    }

    fn update_monitor_profile_active_scene(&mut self, scene_id: &str) {
        let Some(target_mon) = resolve_effective_target_monitor_id(
            self.scene_target(scene_id),
            self.launcher_monitor(),
        ) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakeWindowSystem;
    use std::fs;

    fn test_app(name: &str) -> (AppController, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "kiri-app-test-{}-{}-{name}",
            std::process::id(),
            crate::diagnostics::current_unix_ms()
        ));
        let app = AppController::new(
            ConfigManager::new(dir.join("config.json")),
            RecoveryManager::new(dir.join("runtime-state.json")),
            Box::new(FakeWindowSystem::new_dual_monitor()),
        );
        (app, dir)
    }

    #[test]
    fn multi_bar_drag_reset_hide_and_switch_attribution() {
        let (mut app, dir) = test_app("multi-bar");
        app.config.launcher.bar_display_mode = BarDisplayMode::AllMonitors;

        let bars = app.resolve_launcher_bars();
        assert_eq!(bars.len(), 2);

        // Dragging right bar past left edge of right monitor stays clamped to MON-2-TARGET
        let dragged = app
            .on_bar_drag_end("MON-2-TARGET", 1800, 1002, 96, Some((1850, 1015)))
            .unwrap();
        assert_eq!(dragged.monitor_id, "MON-2-TARGET");
        assert_eq!(dragged.x, 1920);
        assert_eq!(dragged.y, 1002);
        assert!(app
            .config
            .launcher
            .positions_by_monitor
            .contains_key("MON-2-TARGET"));

        // Resetting only MON-2-TARGET puts it at default top-right without overwriting MON-1-PRIMARY
        app.on_bar_drag_end("MON-1-PRIMARY", 500, 1002, 96, Some((520, 1015)))
            .unwrap();
        app.reset_bar_position(Some("MON-2-TARGET"));
        let bars_after_reset = app.resolve_launcher_bars();
        assert_eq!(bars_after_reset[0].y, 1002);
        assert!(bars_after_reset[1].y < 100);

        // Hiding MON-1-PRIMARY converts AllMonitors -> SelectedMonitors([MON-2-TARGET])
        assert!(app.hide_bar_on_monitor("MON-1-PRIMARY"));
        assert_eq!(
            app.config.launcher.bar_display_mode,
            BarDisplayMode::SelectedMonitors
        );
        assert_eq!(app.config.launcher.bar_monitor_ids, vec!["MON-2-TARGET"]);
        // Cannot hide the last remaining bar
        assert!(!app.hide_bar_on_monitor("MON-2-TARGET"));

        // Switching from a bar attributes MonitorPolicy::LauncherMonitor to that bar's monitor
        app.config.target.monitor_policy = MonitorPolicy::LauncherMonitor;
        let _ = app.switch_to_scene_from_monitor(
            "code",
            None,
            SwitchSource::BarClick,
            Some("MON-2-TARGET"),
        );
        assert_eq!(app.launcher_monitor_id, "MON-2-TARGET");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn normal_shutdown_leaves_minimized_windows_minimized_and_clears_journal() {
        let dir = std::env::temp_dir().join(format!(
            "kiri-app-test-{}-{}-normal-shutdown",
            std::process::id(),
            crate::diagnostics::current_unix_ms()
        ));
        let journal_path = dir.join("runtime-state.json");

        let mut sys = FakeWindowSystem::new_dual_monitor();
        let win = crate::platform::WindowInstance {
            hwnd: 42,
            pid: 4200,
            process_start_time: 1000,
            exe_path: r"C:\Apps\Test\app.exe".to_string(),
            window_class: "TestWindow".to_string(),
            title: "Test App".to_string(),
            aumid: None,
            monitor_id: "MON-2-TARGET".to_string(),
            is_visible: true,
            is_iconic: false,
            is_topmost: false,
            is_enabled: true,
            is_hung: false,
            is_maximized: false,
            has_taskbar_button: true,
            is_elevated: false,
            normal_rect: crate::config::RectData {
                left: 1920,
                top: 0,
                right: 2920,
                bottom: 800,
            },
            window_rect: crate::config::RectData {
                left: 1920,
                top: 0,
                right: 2920,
                bottom: 800,
            },
            z_order: 0,
        };
        sys.add_window(win.clone());

        let mut app = AppController::new(
            ConfigManager::new(dir.join("config.json")),
            RecoveryManager::new(journal_path.clone()),
            Box::new(sys),
        );

        app.recovery
            .record_pending_minimizes(&[("binding-1".to_string(), win)]);
        app.sys
            .show_window_async(42, crate::platform::ShowCmd::MinimizeNoActivate)
            .unwrap();
        app.recovery.mark_minimize_issued(42);
        app.recovery.persist_journal().unwrap();

        assert!(app.sys.query_window(42).unwrap().is_iconic);
        assert!(journal_path.exists());

        app.on_normal_shutdown();

        assert!(app.sys.query_window(42).unwrap().is_iconic);
        assert!(!journal_path.exists());
        assert!(app.recovery.actions().is_empty());
        assert!(app.check_startup_crash_recovery().is_empty());

        let _ = fs::remove_dir_all(dir);
    }
}
