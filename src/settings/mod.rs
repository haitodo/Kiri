//! Settings Window framework, view-model preparation, and Windows Autostart (`HKCU\...\Run`) helper.
//!
//! Per Section 2.4 & 11 of spec.md:
//! - The Settings window is a normal top-level window opened on demand ("右クリック → 設定" or 2nd instance activation).
//! - When closed, the window and all its GDI/Win32 control resources are destroyed so resident memory stays minimal.
//! - Provides `SettingsViewModel` and helper methods for the 4 tabs:
//!   1. Scenes (`SceneTab`): create/delete/rename scenes, icon, order, `pinned`.
//!   2. Windows (`WindowsTab`): live open window list with `RegistrationWarning` badges & safe `TitleRule` suggestion.
//!   3. Launcher (`LauncherTab`): orientation, button/grip size, visible slots, always-on-top, snap, reset position.
//!   4. Behavior (`BehaviorTab`): target monitor selector, focus_on_switch, hotkeys, autostart registry, recovery.

pub mod window;

use crate::config::{AppConfig, InstanceHint, WindowBinding, WindowMatch};
use crate::platform::{MonitorInfoData, WindowInstance, WindowSystem};
use crate::scenes::bindings::{
    evaluate_registration_warnings, suggest_safe_title_rule, RegistrationWarning,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Scenes,
    Windows,
    Launcher,
    Behavior,
}

#[derive(Debug, Clone)]
pub struct CandidateWindowRow {
    pub instance: WindowInstance,
    pub warnings: Vec<RegistrationWarning>,
    pub bound_scene_ids: Vec<String>,
    pub existing_binding_id: Option<String>,
    pub always_visible: bool,
}

/// Snapshot of data needed to render and edit the 4 tabs of the Settings window.
#[derive(Debug, Clone)]
pub struct SettingsViewModel {
    pub active_tab: SettingsTab,
    pub config_draft: AppConfig,
    pub monitors: Vec<MonitorInfoData>,
    pub candidate_windows: Vec<CandidateWindowRow>,
    pub autostart_enabled: bool,
}

impl SettingsViewModel {
    /// Build a fresh `SettingsViewModel` by enumerating monitors and open windows via `WindowSystem`.
    /// If `target.monitor_id` is not set yet, pre-populates the recommended monitor (launcher's monitor or secondary/primary).
    pub fn build(
        config: &AppConfig,
        sys: &dyn WindowSystem,
        launcher_monitor_id: Option<&str>,
    ) -> Self {
        let mut draft = config.clone();
        let monitors = sys.monitors();

        if draft.target.monitor_id.is_none() {
            let recommended = launcher_monitor_id
                .map(|s| s.to_string())
                .or_else(|| {
                    monitors
                        .iter()
                        .find(|m| !m.is_primary)
                        .or_else(|| monitors.first())
                        .map(|m| m.monitor_id.clone())
                });
            draft.target.monitor_id = recommended;
        }

        let mut vm = Self {
            active_tab: SettingsTab::Windows,
            config_draft: draft,
            monitors,
            candidate_windows: Vec::new(),
            autostart_enabled: is_autostart_enabled(),
        };
        vm.refresh_candidates(sys);
        vm
    }

    /// Re-enumerate live windows from `sys` and match against `self.config_draft.bindings`.
    pub fn refresh_candidates(&mut self, sys: &dyn WindowSystem) {
        self.monitors = sys.monitors();
        let target_mon_ref = self.config_draft.target.monitor_id.as_deref();
        let open_windows = sys.enumerate_windows();

        self.candidate_windows = open_windows
            .into_iter()
            .map(|win| {
                let warnings = evaluate_registration_warnings(&win, target_mon_ref);
                CandidateWindowRow {
                    instance: win,
                    warnings,
                    bound_scene_ids: Vec::new(),
                    existing_binding_id: None,
                    always_visible: false,
                }
            })
            .collect();
        self.sync_binding_annotations();
    }

    /// Re-synchronize `bound_scene_ids`, `existing_binding_id`, and `always_visible` for each candidate row
    /// against `self.config_draft.bindings` without needing a full window re-enumeration.
    pub fn sync_binding_annotations(&mut self) {
        let bindings = &self.config_draft.bindings;
        for row in &mut self.candidate_windows {
            let existing = bindings
                .iter()
                .find(|b| {
                    b.enabled
                        && b.instance_hint
                            .as_ref()
                            .map(|h| {
                                h.last_pid == Some(row.instance.pid)
                                    && h.last_start_time == Some(row.instance.process_start_time)
                            })
                            .unwrap_or(false)
                })
                .or_else(|| {
                    bindings.iter().find(|b| {
                        b.enabled
                            && crate::scenes::bindings::matches_window_rule(
                                &b.match_rule,
                                &row.instance,
                            )
                    })
                });

            if let Some(b) = existing {
                row.bound_scene_ids = b.scene_ids.clone();
                row.existing_binding_id = Some(b.id.clone());
                row.always_visible = b.always_visible;
            } else {
                row.bound_scene_ids.clear();
                row.existing_binding_id = None;
                row.always_visible = false;
            }
        }
    }

    /// Helper to register or update a `WindowInstance` into `scene_id` with a safe `TitleRule` and explicit `InstanceHint`.
    pub fn bind_window_to_scene(
        &mut self,
        win: &WindowInstance,
        scene_id: &str,
        always_visible: bool,
    ) -> String {
        if let Some(existing) = self.config_draft.bindings.iter_mut().find(|b| {
            b.instance_hint
                .as_ref()
                .map(|h| {
                    h.last_pid == Some(win.pid) && h.last_start_time == Some(win.process_start_time)
                })
                .unwrap_or(false)
        }) {
            if !existing.scene_ids.iter().any(|s| s == scene_id) {
                existing.scene_ids.push(scene_id.to_string());
            }
            existing.always_visible = always_visible;
            existing.enabled = true;
            let id = existing.id.clone();
            self.sync_binding_annotations();
            return id;
        }

        let binding_id = format!("bind-{}-{:x}", win.pid, win.hwnd);
        let binding = WindowBinding {
            id: binding_id.clone(),
            match_rule: WindowMatch {
                exe_path: win.exe_path.clone(),
                window_class: win.window_class.clone(),
                title_rule: suggest_safe_title_rule(win),
                aumid: win.aumid.clone(),
            },
            instance_hint: Some(InstanceHint {
                last_pid: Some(win.pid),
                last_start_time: Some(win.process_start_time),
                disambiguator: Some("confirmed_target_monitor".to_string()),
            }),
            scene_ids: vec![scene_id.to_string()],
            always_visible,
            enabled: true,
        };
        self.config_draft.bindings.push(binding);
        self.sync_binding_annotations();
        binding_id
    }
}

/// Check if `SceneDock` is registered in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (FR-14, Section 11).
#[cfg(windows)]
pub fn is_autostart_enabled() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return false;
        }
        let exists = RegQueryValueExW(hkey, w!("SceneDock"), None, None, None, None).is_ok();
        let _ = RegCloseKey(hkey);
        exists
    }
}

#[cfg(not(windows))]
pub fn is_autostart_enabled() -> bool {
    false
}

/// Enable or disable Windows logon autostart in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (FR-14, Section 11).
#[cfg(windows)]
pub fn set_autostart_enabled(enable: bool) -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    };

    unsafe {
        let mut hkey = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .ok()
        .map_err(|e| format!("RegOpenKeyExW failed: {}", e))?;

        let res = if enable {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let quoted = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
            let byte_slice =
                std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            RegSetValueExW(hkey, w!("SceneDock"), 0, REG_SZ, Some(byte_slice))
                .ok()
                .map_err(|e| format!("RegSetValueExW failed: {}", e))
        } else {
            let _ = RegDeleteValueW(hkey, w!("SceneDock"));
            Ok(())
        };

        let _ = RegCloseKey(hkey);
        res
    }
}
