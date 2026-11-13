//! Settings Window framework, view-model preparation, and Windows Autostart (`HKCU\...\Run`) helper.
//!
//! Per Section 2.4 & 11 of spec.md:
//! - The Settings window is a normal top-level window opened on demand ("右クリック → 設定" or 2nd instance activation).
//! - When closed, the window and all its GDI/Win32 control resources are destroyed so resident memory stays minimal.
//! - Provides `SettingsViewModel` and helper methods for the settings tabs:
//!   1. Scenes (`SceneTab`): create/delete/rename scenes, icon, order, `pinned`.
//!   2. Windows (`WindowsTab`): live open window list with `RegistrationWarning` badges & safe `TitleRule` suggestion.
//!   3. Launcher (`LauncherTab`): orientation, button/grip size, visible slots, always-on-top, snap, reset position.
//!   4. Behavior (`BehaviorTab`): target monitor selector, focus_on_switch, hotkeys, autostart registry, recovery.
//!   5. Shortcuts: register and reorder folders, files, and sites.

pub mod window;

use crate::config::{AppConfig, InstanceHint, MonitorPolicy, TitleRule, TitleRuleType, WindowBinding, WindowMatch};
use crate::platform::{MonitorInfoData, WindowInstance, WindowSystem};
use crate::scenes::bindings::{
    evaluate_registration_warnings, exe_paths_equal, extract_smart_title_keyword,
    suggest_safe_title_rule, RegistrationWarning,
};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Scenes,
    Windows,
    Launcher,
    Behavior,
    Shortcuts,
}

#[derive(Debug, Clone)]
pub struct CandidateWindowRow {
    pub instance: WindowInstance,
    pub warnings: Vec<RegistrationWarning>,
    pub bound_scene_ids: Vec<String>,
    pub existing_binding_id: Option<String>,
    pub always_visible: bool,
}

/// Represents an application binding row in the matrix table (FR-06, FR-07).
/// Displays both running and non-running registered applications.
#[derive(Debug, Clone)]
pub struct AppBindingRow {
    pub binding_id: String,
    pub is_running: bool,
    pub live_hwnd: Option<u64>,
    pub live_pid: Option<u32>,
    pub exe_name: String,
    pub exe_path: String,
    pub window_class: String,
    pub window_title: String,
    pub custom_title_keyword: String,
    pub monitor_id: Option<String>,
    pub monitor_label: String,
    pub bound_scene_ids: Vec<String>,
    pub always_visible: bool,
    pub warnings: Vec<RegistrationWarning>,
}

/// Snapshot of data needed to render and edit the Settings window.
#[derive(Debug, Clone)]
pub struct SettingsViewModel {
    pub active_tab: SettingsTab,
    pub config_draft: AppConfig,
    pub monitors: Vec<MonitorInfoData>,
    pub candidate_windows: Vec<CandidateWindowRow>,
    pub binding_rows: Vec<AppBindingRow>,
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
            binding_rows: Vec::new(),
            autostart_enabled: is_autostart_enabled(),
        };
        vm.refresh_candidates(sys);
        vm
    }

    /// Re-enumerate live windows from `sys` and match against `self.config_draft.bindings`.
    pub fn refresh_candidates(&mut self, sys: &dyn WindowSystem) {
        self.monitors = sys.monitors();
        let target_mon_ref = if self.config_draft.target.monitor_policy == MonitorPolicy::AllMonitors
        {
            None
        } else {
            self.config_draft.target.monitor_id.as_deref()
        };
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
        self.sync_binding_rows();
    }

    /// Return a user-friendly position label for a monitor (e.g. "左モニター", "右モニター").
    pub fn format_monitor_short_label(&self, monitor_id: &str) -> String {
        if self.monitors.len() >= 2 {
            let mut sorted: Vec<&MonitorInfoData> = self.monitors.iter().collect();
            sorted.sort_by_key(|m| (m.work_area.left, m.work_area.top));
            if let Some(pos) = sorted.iter().position(|m| m.monitor_id == monitor_id) {
                if sorted.len() == 2 {
                    return if pos == 0 {
                        "左モニター".to_string()
                    } else {
                        "右モニター".to_string()
                    };
                } else if pos == 0 {
                    return "左モニター".to_string();
                } else if pos + 1 == sorted.len() {
                    return "右モニター".to_string();
                } else {
                    return format!("中央モニター{}", pos);
                }
            }
        }
        if let Some(m) = self.monitors.iter().find(|m| m.monitor_id == monitor_id) {
            if !m.friendly_name.is_empty() {
                return m.friendly_name.clone();
            }
        }
        monitor_id.to_string()
    }

    /// Suggest an initial title keyword when selecting an unregistered window:
    /// - If multiple windows of the same executable are on the same monitor (or another binding for the same
    ///   executable already exists on that monitor), extracts the leading page/document title keyword.
    /// - Otherwise returns `None` so the window is matched purely by executable + monitor placement.
    pub fn suggest_default_title_keyword(&self, win: &WindowInstance) -> Option<String> {
        let same_exe_on_mon_count = self
            .candidate_windows
            .iter()
            .filter(|r| {
                r.instance.monitor_id == win.monitor_id
                    && exe_paths_equal(&r.instance.exe_path, &win.exe_path)
            })
            .count();

        let existing_same_exe_on_mon = self.config_draft.bindings.iter().any(|b| {
            b.enabled
                && exe_paths_equal(&b.match_rule.exe_path, &win.exe_path)
                && (b.match_rule.monitor_id.as_deref() == Some(win.monitor_id.as_str())
                    || b.instance_hint
                        .as_ref()
                        .and_then(|h| h.last_monitor_id.as_deref())
                        == Some(win.monitor_id.as_str()))
        });

        if same_exe_on_mon_count > 1 || existing_same_exe_on_mon {
            extract_smart_title_keyword(&win.title)
        } else {
            None
        }
    }

    /// Re-synchronize `bound_scene_ids`, `existing_binding_id`, and `always_visible` for each candidate row
    /// against `self.config_draft.bindings` without needing a full window re-enumeration.
    pub fn sync_binding_annotations(&mut self) {
        let bindings = &self.config_draft.bindings;
        let mut claimed_binding_ids: HashSet<String> = HashSet::new();

        for row in &mut self.candidate_windows {
            row.bound_scene_ids.clear();
            row.existing_binding_id = None;
            row.always_visible = false;
        }

        // Pass 1: Exact runtime identity match `(last_hwnd, last_pid, last_start_time)`
        for row in &mut self.candidate_windows {
            if let Some(b) = bindings.iter().find(|b| {
                b.enabled
                    && !claimed_binding_ids.contains(&b.id)
                    && b.instance_hint
                        .as_ref()
                        .map(|h| {
                            h.last_hwnd == Some(row.instance.hwnd)
                                && h.last_pid == Some(row.instance.pid)
                                && h.last_start_time == Some(row.instance.process_start_time)
                        })
                        .unwrap_or(false)
            }) {
                row.bound_scene_ids = b.scene_ids.clone();
                row.existing_binding_id = Some(b.id.clone());
                row.always_visible = b.always_visible;
                claimed_binding_ids.insert(b.id.clone());
            }
        }

        // Pass 2: Rule + monitor placement match (preferring specific title rules first)
        for require_specific_title in [true, false] {
            for row in &mut self.candidate_windows {
                if row.existing_binding_id.is_some() {
                    continue;
                }
                if let Some(b) = bindings.iter().find(|b| {
                    if !b.enabled || claimed_binding_ids.contains(&b.id) {
                        return false;
                    }
                    let is_specific = b.match_rule.title_rule.rule_type != TitleRuleType::Any;
                    if is_specific != require_specific_title {
                        return false;
                    }
                    if let Some(ref req_mon) = b.match_rule.monitor_id {
                        if req_mon != &row.instance.monitor_id {
                            return false;
                        }
                    }
                    crate::scenes::bindings::matches_window_rule(&b.match_rule, &row.instance)
                }) {
                    row.bound_scene_ids = b.scene_ids.clone();
                    row.existing_binding_id = Some(b.id.clone());
                    row.always_visible = b.always_visible;
                    claimed_binding_ids.insert(b.id.clone());
                }
            }
        }
    }

    /// Register or update a `WindowInstance` into `scene_id` with default options.
    pub fn bind_window_to_scene(
        &mut self,
        win: &WindowInstance,
        scene_id: &str,
        always_visible: bool,
    ) -> String {
        let suggested_kw = self.suggest_default_title_keyword(win);
        self.bind_window_to_scene_with_options(win, scene_id, always_visible, suggested_kw, true)
    }

    /// Register or update a `WindowInstance` into `scene_id` with explicit title keyword and monitor pinning.
    pub fn bind_window_to_scene_with_options(
        &mut self,
        win: &WindowInstance,
        scene_id: &str,
        always_visible: bool,
        custom_title_keyword: Option<String>,
        pin_to_monitor: bool,
    ) -> String {
        let title_rule = match custom_title_keyword
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            Some(kw) => TitleRule::contains(kw),
            None => suggest_safe_title_rule(win),
        };
        let monitor_id_rule = if pin_to_monitor {
            Some(win.monitor_id.clone())
        } else {
            None
        };

        // Find existing binding for this exact window (by HWND or synced binding ID)
        let synced_binding_id = self
            .candidate_windows
            .iter()
            .find(|r| r.instance.hwnd == win.hwnd)
            .and_then(|r| r.existing_binding_id.clone());

        if let Some(existing) = self.config_draft.bindings.iter_mut().find(|b| {
            let exact_hwnd = b
                .instance_hint
                .as_ref()
                .map(|h| {
                    h.last_hwnd == Some(win.hwnd)
                        && h.last_pid == Some(win.pid)
                        && h.last_start_time == Some(win.process_start_time)
                })
                .unwrap_or(false);
            exact_hwnd || synced_binding_id.as_deref() == Some(b.id.as_str())
        }) {
            if !existing.scene_ids.iter().any(|s| s == scene_id) {
                existing.scene_ids.push(scene_id.to_string());
            }
            existing.match_rule.title_rule = title_rule;
            existing.match_rule.monitor_id = monitor_id_rule;
            existing.instance_hint = Some(InstanceHint {
                last_hwnd: Some(win.hwnd),
                last_pid: Some(win.pid),
                last_start_time: Some(win.process_start_time),
                last_monitor_id: Some(win.monitor_id.clone()),
                disambiguator: Some("confirmed_target_monitor".to_string()),
            });
            existing.always_visible = always_visible;
            existing.enabled = true;
            let id = existing.id.clone();
            self.sync_binding_annotations();
            self.sync_binding_rows();
            return id;
        }

        // If a sibling binding for the same executable on the same monitor currently uses `TitleRuleType::Any`,
        // upgrade its title rule using its live window's title keyword so both windows can be disambiguated after restart.
        let candidates_snapshot = self.candidate_windows.clone();
        for other in &mut self.config_draft.bindings {
            if other.enabled
                && other.match_rule.title_rule.rule_type == TitleRuleType::Any
                && exe_paths_equal(&other.match_rule.exe_path, &win.exe_path)
                && other.match_rule.monitor_id.as_deref() == Some(win.monitor_id.as_str())
            {
                if let Some(other_hwnd) = other.instance_hint.as_ref().and_then(|h| h.last_hwnd) {
                    if other_hwnd != win.hwnd {
                        if let Some(live_row) = candidates_snapshot
                            .iter()
                            .find(|r| r.instance.hwnd == other_hwnd)
                        {
                            if let Some(kw) = extract_smart_title_keyword(&live_row.instance.title)
                            {
                                other.match_rule.title_rule = TitleRule::contains(kw);
                            }
                        }
                    }
                }
            }
        }

        let binding_id = format!("bind-{}-{:x}", win.pid, win.hwnd);
        let binding = WindowBinding {
            id: binding_id.clone(),
            match_rule: WindowMatch {
                exe_path: win.exe_path.clone(),
                window_class: win.window_class.clone(),
                title_rule,
                aumid: win.aumid.clone(),
                monitor_id: monitor_id_rule,
            },
            instance_hint: Some(InstanceHint {
                last_hwnd: Some(win.hwnd),
                last_pid: Some(win.pid),
                last_start_time: Some(win.process_start_time),
                last_monitor_id: Some(win.monitor_id.clone()),
                disambiguator: Some("confirmed_target_monitor".to_string()),
            }),
            scene_ids: vec![scene_id.to_string()],
            always_visible,
            enabled: true,
        };
        self.config_draft.bindings.push(binding);
        self.sync_binding_annotations();
        self.sync_binding_rows();
        binding_id
    }

    /// Remove `scene_id` from a binding. If the binding has no remaining scenes, remove the binding itself.
    pub fn unbind_window_from_scene(&mut self, binding_id: &str, scene_id: &str) {
        if let Some(b) = self
            .config_draft
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
        {
            b.scene_ids.retain(|s| s != scene_id);
        }
        self.config_draft
            .bindings
            .retain(|b| b.id != binding_id || !b.scene_ids.is_empty());
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }

    /// Synchronize the view-model `binding_rows` by matching `config_draft.bindings`
    /// against the current `candidate_windows`.
    pub fn sync_binding_rows(&mut self) {
        let mut rows = Vec::new();
        for b in &self.config_draft.bindings {
            let matching_cand = self
                .candidate_windows
                .iter()
                .find(|c| c.existing_binding_id.as_deref() == Some(&b.id));

            let is_running = matching_cand.is_some();
            let live_hwnd = matching_cand.map(|c| c.instance.hwnd);
            let live_pid = matching_cand.map(|c| c.instance.pid);
            let warnings = matching_cand.map(|c| c.warnings.clone()).unwrap_or_default();

            let exe_name = std::path::Path::new(&b.match_rule.exe_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&b.match_rule.exe_path)
                .to_string();

            let window_title = if let Some(cand) = matching_cand {
                if cand.instance.title.is_empty() {
                    "(タイトルなし)".to_string()
                } else {
                    cand.instance.title.clone()
                }
            } else if let Some(ref kw) = b.match_rule.title_rule.value {
                format!("\"{}\" (未起動)", kw)
            } else {
                "(未起動)".to_string()
            };

            let custom_title_keyword = b
                .match_rule
                .title_rule
                .value
                .clone()
                .unwrap_or_default();

            let monitor_id = b.match_rule.monitor_id.clone().or_else(|| {
                matching_cand.map(|c| c.instance.monitor_id.clone())
            });

            let monitor_label = match &b.match_rule.monitor_id {
                Some(m) => self.format_monitor_short_label(m),
                None => match matching_cand {
                    Some(cand) => format!("{}(不問)", self.format_monitor_short_label(&cand.instance.monitor_id)),
                    None => "全画面".to_string(),
                },
            };

            rows.push(AppBindingRow {
                binding_id: b.id.clone(),
                is_running,
                live_hwnd,
                live_pid,
                exe_name,
                exe_path: b.match_rule.exe_path.clone(),
                window_class: b.match_rule.window_class.clone(),
                window_title,
                custom_title_keyword,
                monitor_id,
                monitor_label,
                bound_scene_ids: b.scene_ids.clone(),
                always_visible: b.always_visible,
                warnings,
            });
        }
        self.binding_rows = rows;
    }

    /// Toggle membership of a binding in a specific scene.
    pub fn toggle_scene_binding(&mut self, binding_id: &str, scene_id: &str) {
        if let Some(b) = self
            .config_draft
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
        {
            if let Some(pos) = b.scene_ids.iter().position(|s| s == scene_id) {
                b.scene_ids.remove(pos);
            } else {
                b.scene_ids.push(scene_id.to_string());
            }
        }
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }

    /// Toggle always_visible flag on a binding.
    pub fn toggle_always_visible(&mut self, binding_id: &str) {
        if let Some(b) = self
            .config_draft
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
        {
            b.always_visible = !b.always_visible;
        }
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }

    /// Remove a binding completely from configuration.
    pub fn remove_binding(&mut self, binding_id: &str) {
        self.config_draft.bindings.retain(|b| b.id != binding_id);
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }

    /// Update title keyword rule on an existing binding.
    pub fn update_binding_title_keyword(&mut self, binding_id: &str, keyword: Option<String>) {
        if let Some(b) = self
            .config_draft
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
        {
            let kw_trimmed = keyword.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
            b.match_rule.title_rule = match kw_trimmed {
                Some(kw) => TitleRule::contains(kw),
                None => TitleRule::any(),
            };
        }
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }

    /// Update monitor restriction rule on an existing binding.
    pub fn update_binding_monitor_pin(&mut self, binding_id: &str, pin_to_monitor: bool, target_mon: Option<String>) {
        if let Some(b) = self
            .config_draft
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
        {
            b.match_rule.monitor_id = if pin_to_monitor {
                target_mon.or_else(|| {
                    b.instance_hint
                        .as_ref()
                        .and_then(|h| h.last_monitor_id.clone())
                })
            } else {
                None
            };
        }
        self.sync_binding_annotations();
        self.sync_binding_rows();
    }
}

/// Check if `Kiri` is registered in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (FR-14, Section 11).
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
        let exists = RegQueryValueExW(hkey, w!("Kiri"), None, None, None, None).is_ok();
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
            RegSetValueExW(hkey, w!("Kiri"), 0, REG_SZ, Some(byte_slice))
                .ok()
                .map_err(|e| format!("RegSetValueExW failed: {}", e))
        } else {
            let _ = RegDeleteValueW(hkey, w!("Kiri"));
            Ok(())
        };

        let _ = RegCloseKey(hkey);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, InstanceHint, RectData, Scene, TitleRule, WindowBinding, WindowMatch};
    use crate::platform::fake::FakeWindowSystem;
    use crate::platform::WindowInstance;

    fn make_test_win(
        hwnd: u64,
        pid: u32,
        exe: &str,
        class: &str,
        title: &str,
        monitor_id: &str,
    ) -> WindowInstance {
        WindowInstance {
            hwnd,
            pid,
            process_start_time: 1000 + pid as u64,
            exe_path: exe.to_string(),
            window_class: class.to_string(),
            title: title.to_string(),
            aumid: None,
            monitor_id: monitor_id.to_string(),
            is_visible: true,
            is_iconic: false,
            is_topmost: false,
            is_enabled: true,
            is_hung: false,
            is_maximized: false,
            has_taskbar_button: true,
            is_elevated: false,
            normal_rect: RectData {
                left: 100,
                top: 100,
                right: 800,
                bottom: 600,
            },
            window_rect: RectData {
                left: 100,
                top: 100,
                right: 800,
                bottom: 600,
            },
            z_order: 0,
        }
    }

    #[test]
    fn test_settings_view_model_matrix_rows_and_operations() {
        let mut config = AppConfig::default_initial();
        config.scenes = vec![
            Scene {
                id: "trade".to_string(),
                name: "Trade".to_string(),
                icon: "chart".to_string(),
                order: 1,
                pinned: true,
            },
            Scene {
                id: "code".to_string(),
                name: "Code".to_string(),
                icon: "code".to_string(),
                order: 2,
                pinned: true,
            },
        ];
        config.bindings = vec![
            WindowBinding {
                id: "bind-mt5".to_string(),
                match_rule: WindowMatch {
                    exe_path: "C:\\MT5\\terminal64.exe".to_string(),
                    window_class: "MetaTrader5".to_string(),
                    title_rule: TitleRule::any(),
                    aumid: None,
                    monitor_id: Some("MON-2-TARGET".to_string()),
                },
                instance_hint: Some(InstanceHint {
                    last_hwnd: Some(0x100),
                    last_pid: Some(1001),
                    last_start_time: Some(2001),
                    last_monitor_id: Some("MON-2-TARGET".to_string()),
                    disambiguator: None,
                }),
                scene_ids: vec!["trade".to_string()],
                always_visible: false,
                enabled: true,
            },
            WindowBinding {
                id: "bind-offline".to_string(),
                match_rule: WindowMatch {
                    exe_path: "C:\\Tools\\TickScope.exe".to_string(),
                    window_class: "TickScopeClass".to_string(),
                    title_rule: TitleRule::contains("Main"),
                    aumid: None,
                    monitor_id: Some("MON-2-TARGET".to_string()),
                },
                instance_hint: None,
                scene_ids: vec!["trade".to_string()],
                always_visible: true,
                enabled: true,
            },
        ];

        let mut sys = FakeWindowSystem::new_dual_monitor();
        // Only MT5 is running; TickScope is closed
        sys.add_window(make_test_win(
            0x100,
            1001,
            "C:\\MT5\\terminal64.exe",
            "MetaTrader5",
            "MT5 - EURUSD",
            "MON-2-TARGET",
        ));
        // An open window not yet registered: VS Code
        sys.add_window(make_test_win(
            0x200,
            2002,
            "C:\\VSCode\\Code.exe",
            "Chrome_WidgetWin_1",
            "Kiri - Visual Studio Code",
            "MON-2-TARGET",
        ));

        let mut vm = SettingsViewModel::build(&config, &sys, Some("MON-2-TARGET"));

        // 1. Verify matrix binding_rows has 2 apps (running MT5 and non-running TickScope)
        assert_eq!(vm.binding_rows.len(), 2);
        let mt5_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-mt5").unwrap();
        assert!(mt5_row.is_running);
        assert_eq!(mt5_row.window_title, "MT5 - EURUSD");
        assert_eq!(mt5_row.bound_scene_ids, vec!["trade"]);
        assert!(!mt5_row.always_visible);

        let offline_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-offline").unwrap();
        assert!(!offline_row.is_running);
        assert!(offline_row.always_visible);
        assert_eq!(offline_row.custom_title_keyword, "Main");

        // 2. Toggle MT5 into "code" scene as well (multiple scene binding)
        vm.toggle_scene_binding("bind-mt5", "code");
        let mt5_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-mt5").unwrap();
        assert!(mt5_row.bound_scene_ids.contains(&"trade".to_string()));
        assert!(mt5_row.bound_scene_ids.contains(&"code".to_string()));

        // Toggle MT5 out of "trade" scene
        vm.toggle_scene_binding("bind-mt5", "trade");
        let mt5_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-mt5").unwrap();
        assert!(!mt5_row.bound_scene_ids.contains(&"trade".to_string()));
        assert_eq!(mt5_row.bound_scene_ids, vec!["code"]);

        // 3. Toggle always_visible
        vm.toggle_always_visible("bind-mt5");
        let mt5_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-mt5").unwrap();
        assert!(mt5_row.always_visible);

        // 4. Update title keyword
        vm.update_binding_title_keyword("bind-mt5", Some("EURUSD".to_string()));
        let mt5_row = vm.binding_rows.iter().find(|r| r.binding_id == "bind-mt5").unwrap();
        assert_eq!(mt5_row.custom_title_keyword, "EURUSD");

        // 5. Add new window (VS Code) to "code" scene
        let vscode_instance = vm
            .candidate_windows
            .iter()
            .find(|c| c.instance.hwnd == 0x200)
            .unwrap()
            .instance
            .clone();
        let new_id = vm.bind_window_to_scene_with_options(
            &vscode_instance,
            "code",
            false,
            Some("Kiri".to_string()),
            true,
        );
        assert_eq!(vm.binding_rows.len(), 3);
        let vscode_row = vm.binding_rows.iter().find(|r| r.binding_id == new_id).unwrap();
        assert!(vscode_row.is_running);
        assert_eq!(vscode_row.exe_name, "Code.exe");
        assert_eq!(vscode_row.bound_scene_ids, vec!["code"]);

        // 6. Remove offline binding
        vm.remove_binding("bind-offline");
        assert_eq!(vm.binding_rows.len(), 2);
        assert!(vm.binding_rows.iter().all(|r| r.binding_id != "bind-offline"));
    }
}
