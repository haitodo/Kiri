//! Central application state and coordinator (`AppController`).
//!
//! Coordinates `ConfigManager`, `SceneStateManager`, `BindingResolver`, `RecoveryManager`,
//! and `WindowSystem` without coupling the core logic to Win32 `HWND` callbacks.

use crate::config::{AppConfig, ConfigManager, MonitorProfile, Orientation};
use crate::diagnostics::{log_info, log_warn};
use crate::launcher::input::LauncherLayout;
use crate::platform::hooks::drain_minimize_events;
use crate::platform::monitors::{compute_saved_position, resolve_launcher_position};
use crate::platform::WindowSystem;
use crate::recovery::{ManagedAction, RecoveryManager};
use crate::scenes::bindings::BindingResolver;
use crate::scenes::layouts::{
    apply_saved_scene_layout, capture_scene_layout, save_layout_into_config,
};
use crate::scenes::manager::SceneStateManager;
use crate::scenes::switch::{
    execute_scene_switch, resolve_effective_target_monitor_id, verify_switch_results,
    PendingVerification, SwitchOutcome, SwitchWarning,
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

    /// Persist launcher position when drag finishes (`WM_EXITSIZEMOVE`, Section 2.2).
    pub fn on_launcher_drag_end(&mut self, screen_x: i32, screen_y: i32) -> (i32, i32) {
        let monitors = self.sys.monitors();
        let layout = self.compute_layout(self.launcher_dpi);
        if let Some(saved) = compute_saved_position(
            screen_x,
            screen_y,
            layout.width_px,
            layout.height_px,
            &monitors,
            self.config.launcher.snap_to_edges,
        ) {
            self.launcher_monitor_id = saved.monitor_id.clone();
            self.launcher_dpi = saved.dpi;
            self.config.launcher.position = Some(saved);
            let _ = self.config_mgr.save(&self.config);
        }
        let (x, y, _, _) = self.resolve_initial_or_updated_launcher_rect();
        (x, y)
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
    pub fn switch_to_scene(&mut self, scene_id: &str) -> SwitchOutcome {
        self.process_queued_win_events();

        let launcher_mon = if self.launcher_monitor_id.is_empty() {
            None
        } else {
            Some(self.launcher_monitor_id.as_str())
        };

        let outcome = execute_scene_switch(
            scene_id,
            &self.config,
            &mut self.state_mgr,
            &mut self.resolver,
            &mut self.recovery,
            &mut *self.sys,
            launcher_mon,
        );

        if outcome.switched {
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
        self.process_queued_win_events();
        self.pending_verification = None;
        let count = self.recovery.restore_all_minimized_by_us(&mut *self.sys);
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
        let launcher_mon = if self.launcher_monitor_id.is_empty() {
            None
        } else {
            Some(self.launcher_monitor_id.as_str())
        };
        let Some(target_mon) = resolve_effective_target_monitor_id(&self.config, launcher_mon)
        else {
            return false;
        };

        if let Some(snapshot) = capture_scene_layout(
            &scene_id,
            &target_mon,
            &self.config,
            &mut self.resolver,
            &*self.sys,
        ) {
            save_layout_into_config(&mut self.config, snapshot);
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
        let launcher_mon = if self.launcher_monitor_id.is_empty() {
            None
        } else {
            Some(self.launcher_monitor_id.as_str())
        };
        let Some(target_mon) = resolve_effective_target_monitor_id(&self.config, launcher_mon)
        else {
            return 0;
        };

        apply_saved_scene_layout(
            &scene_id,
            &target_mon,
            &self.config,
            &mut self.resolver,
            &mut *self.sys,
        )
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
