//! Configuration data model, schema validation, migration, backup fallback,
//! and Win32 atomic file replacement (`FlushFileBuffers` + `ReplaceFileW` / `MoveFileExW`).

use crate::diagnostics::{log_error, log_info, log_warn};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Default for Orientation {
    fn default() -> Self {
        Orientation::Horizontal
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorPopupPosition {
    #[default]
    SceneCentered,
    BesideCursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPosition {
    /// Stable monitor identifier (from QueryDisplayConfig target path or fallback key)
    pub monitor_id: String,
    /// Relative X within monitor work area (`0.0..=1.0` ratio or DIP offset from work area left)
    pub rel_x: f64,
    /// Relative Y within monitor work area (`0.0..=1.0` ratio or DIP offset from work area top)
    pub rel_y: f64,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
}

fn default_dpi() -> u32 {
    96
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LauncherConfig {
    #[serde(default = "default_true")]
    pub show_bar: bool,
    #[serde(default = "default_true")]
    pub ctrl_double_tap: bool,
    #[serde(default)]
    pub cursor_popup_position: CursorPopupPosition,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default = "default_button_size_dip")]
    pub button_size_dip: i32,
    #[serde(default = "default_grip_size_dip")]
    pub grip_size_dip: i32,
    #[serde(default = "default_visible_slots")]
    pub visible_slots: usize,
    #[serde(default = "default_true")]
    pub always_on_top: bool,
    #[serde(default)]
    pub snap_to_edges: bool,
    #[serde(default = "default_true")]
    pub focus_on_switch: bool,
    /// When switching to a scene, restore ALL registered windows that are currently iconic,
    /// regardless of whether they were minimized by Kiri or manually by the user.
    #[serde(default = "default_true")]
    pub restore_all_on_switch: bool,
    #[serde(default)]
    pub position: Option<SavedPosition>,
}

fn default_button_size_dip() -> i32 {
    32
}
fn default_grip_size_dip() -> i32 {
    18
}
fn default_visible_slots() -> usize {
    3
}
fn default_true() -> bool {
    true
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            show_bar: true,
            ctrl_double_tap: true,
            cursor_popup_position: CursorPopupPosition::default(),
            orientation: Orientation::Horizontal,
            button_size_dip: 32,
            grip_size_dip: 18,
            visible_slots: 3,
            always_on_top: true,
            snap_to_edges: false,
            focus_on_switch: true,
            restore_all_on_switch: true,
            position: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorPolicy {
    AllMonitors,
    Fixed,
    LauncherMonitor,
}

impl Default for MonitorPolicy {
    fn default() -> Self {
        MonitorPolicy::AllMonitors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetConfig {
    #[serde(default)]
    pub monitor_policy: MonitorPolicy,
    #[serde(default)]
    pub monitor_id: Option<String>,
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            monitor_policy: MonitorPolicy::AllMonitors,
            monitor_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub order: i32,
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutKind {
    Folder,
    File,
    Website,
}

impl ShortcutKind {
    pub const ALL: [Self; 3] = [Self::Folder, Self::File, Self::Website];

    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "フォルダー",
            Self::File => "ファイル",
            Self::Website => "サイト",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub name: String,
    pub kind: ShortcutKind,
    pub target: String,
}

impl Shortcut {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() || self.name.chars().any(char::is_control) {
            return Err("ショートカットの表示名を入力してください。");
        }
        if self.target.chars().any(char::is_control) {
            return Err("場所に改行などの制御文字は使えません。");
        }
        match self.kind {
            ShortcutKind::Folder | ShortcutKind::File if !Path::new(&self.target).is_absolute() => {
                Err("フォルダーまたはファイルの絶対パスを入力してください。")
            }
            ShortcutKind::Website => {
                let valid = self.target.split_once("://").is_some_and(|(scheme, rest)| {
                    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
                        && !rest.split(['/', '?', '#']).next().unwrap_or_default().is_empty()
                        && !self.target.chars().any(char::is_whitespace)
                });
                valid.then_some(()).ok_or("http:// または https:// で始まる URL を入力してください。")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleRuleType {
    Any,
    Exact,
    Prefix,
    Contains,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleRule {
    #[serde(rename = "type")]
    pub rule_type: TitleRuleType,
    #[serde(default)]
    pub value: Option<String>,
}

impl TitleRule {
    pub fn any() -> Self {
        Self {
            rule_type: TitleRuleType::Any,
            value: None,
        }
    }

    pub fn contains(val: impl Into<String>) -> Self {
        Self {
            rule_type: TitleRuleType::Contains,
            value: Some(val.into()),
        }
    }

    pub fn prefix(val: impl Into<String>) -> Self {
        Self {
            rule_type: TitleRuleType::Prefix,
            value: Some(val.into()),
        }
    }

    pub fn exact(val: impl Into<String>) -> Self {
        Self {
            rule_type: TitleRuleType::Exact,
            value: Some(val.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMatch {
    pub exe_path: String,
    pub window_class: String,
    pub title_rule: TitleRule,
    #[serde(default)]
    pub aumid: Option<String>,
    /// Optional monitor placement rule (e.g. left vs right monitor).
    /// When set, re-matching after process restart matches windows on this monitor.
    #[serde(default)]
    pub monitor_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstanceHint {
    #[serde(default)]
    pub last_hwnd: Option<u64>,
    #[serde(default)]
    pub last_pid: Option<u32>,
    #[serde(default)]
    pub last_start_time: Option<u64>,
    #[serde(default)]
    pub last_monitor_id: Option<String>,
    #[serde(default)]
    pub disambiguator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowBinding {
    pub id: String,
    #[serde(rename = "match")]
    pub match_rule: WindowMatch,
    #[serde(default)]
    pub instance_hint: Option<InstanceHint>,
    pub scene_ids: Vec<String>,
    #[serde(default)]
    pub always_visible: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub scene_id: String,
    /// Win32 MOD_ALT (0x1), MOD_CONTROL (0x2), MOD_SHIFT (0x4), MOD_WIN (0x8)
    pub modifiers: u32,
    pub vk: u32,
}

impl HotkeyBinding {
    /// Per Section 7.2: warn when a hotkey has no modifier keys (e.g. bare function key like F9).
    pub fn has_safe_modifiers(&self) -> bool {
        (self.modifiers & 0x000F) != 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindowLayoutEntry {
    pub binding_id: String,
    pub rel_left: f64,
    pub rel_top: f64,
    pub rel_right: f64,
    pub rel_bottom: f64,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub scene_id: String,
    pub monitor_id: String,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    pub entries: Vec<SavedWindowLayoutEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RectData {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectData {
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn intersection_area(&self, other: &RectData) -> i64 {
        let x_overlap = (self.right.min(other.right) - self.left.max(other.left)).max(0) as i64;
        let y_overlap = (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0) as i64;
        x_overlap * y_overlap
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorProfile {
    pub monitor_id: String,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub work_area: RectData,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    #[serde(default)]
    pub last_active_scene_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub launcher: LauncherConfig,
    #[serde(default)]
    pub target: TargetConfig,
    #[serde(default)]
    pub scenes: Vec<Scene>,
    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub bindings: Vec<WindowBinding>,
    #[serde(default)]
    pub hotkeys: Vec<HotkeyBinding>,
    #[serde(default)]
    pub layouts: Vec<LayoutSnapshot>,
    #[serde(default)]
    pub monitors: Vec<MonitorProfile>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::default_initial()
    }
}

impl AppConfig {
    /// Default initial configuration matching Section 6 of spec.md.
    pub fn default_initial() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            launcher: LauncherConfig::default(),
            target: TargetConfig::default(),
            scenes: vec![
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
                Scene {
                    id: "web".to_string(),
                    name: "Web".to_string(),
                    icon: "globe".to_string(),
                    order: 3,
                    pinned: true,
                },
            ],
            shortcuts: Vec::new(),
            bindings: vec![WindowBinding {
                id: "example-mt5".to_string(),
                match_rule: WindowMatch {
                    exe_path: r"C:\Program Files\Example\terminal64.exe".to_string(),
                    window_class: "ExampleClass".to_string(),
                    title_rule: TitleRule::contains("MetaTrader 5"),
                    aumid: None,
                    monitor_id: None,
                },
                instance_hint: None,
                scene_ids: vec!["trade".to_string()],
                always_visible: false,
                enabled: false,
            }],
            hotkeys: Vec::new(),
            layouts: Vec::new(),
            monitors: Vec::new(),
        }
    }

    /// Returns scenes sorted by `(order, id)`.
    pub fn ordered_scenes(&self) -> Vec<&Scene> {
        let mut list: Vec<&Scene> = self.scenes.iter().collect();
        list.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        list
    }

    /// Splits scenes into `(visible_bar_scenes, overflow_scenes)` according to `pinned` and `visible_slots`.
    /// Per Section 2.3:
    /// - Pinned scenes (in manual `order`) fill up to `visible_slots`.
    /// - Any remaining pinned scenes or unpinned scenes go into the `⋯` overflow popup.
    pub fn split_visible_and_overflow(&self) -> (Vec<Scene>, Vec<Scene>) {
        let ordered = self.ordered_scenes();
        let max_slots = self.launcher.visible_slots.max(1);
        let mut visible = Vec::new();
        let mut overflow = Vec::new();

        for scene in ordered {
            if scene.pinned && visible.len() < max_slots {
                visible.push(scene.clone());
            } else {
                overflow.push(scene.clone());
            }
        }
        (visible, overflow)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLoadStatus {
    /// Loaded normally from `config.json`.
    LoadedNormally,
    /// `config.json` did not exist; initialized with default configuration.
    CreatedDefault,
    /// `config.json` was corrupted; recovered from `config.json.bak`.
    RecoveredFromBackup { error: String },
    /// Both `config.json` and `config.json.bak` failed; reset to default.
    ResetAfterCorruption { error: String },
    /// Loaded a config with `schema_version > CURRENT_SCHEMA_VERSION`; read-only mode enabled.
    LoadedReadOnlyFutureSchema { found_version: u32 },
}

pub struct ConfigManager {
    config_path: PathBuf,
    read_only: bool,
    last_status: ConfigLoadStatus,
    warnings: Vec<String>,
}

impl ConfigManager {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            read_only: false,
            last_status: ConfigLoadStatus::CreatedDefault,
            warnings: Vec::new(),
        }
    }

    pub fn default_config_path() -> PathBuf {
        crate::diagnostics::default_app_data_dir().join("config.json")
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn backup_path(&self) -> PathBuf {
        self.config_path.with_extension("json.bak")
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn last_status(&self) -> &ConfigLoadStatus {
        &self.last_status
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Load configuration from disk, handling schema version checks, migrations,
    /// unknown key warnings, and `.bak` fallback on corruption.
    pub fn load(&mut self) -> AppConfig {
        self.warnings.clear();
        self.read_only = false;

        if !self.config_path.exists() {
            log_info("config", "config.json not found; using initial default config");
            self.last_status = ConfigLoadStatus::CreatedDefault;
            let cfg = AppConfig::default_initial();
            let _ = self.save(&cfg);
            return cfg;
        }

        match self.read_and_validate_file(&self.config_path) {
            Ok((mut cfg, file_warnings)) => {
                self.warnings.extend(file_warnings);
                if cfg.schema_version > CURRENT_SCHEMA_VERSION {
                    let msg = format!(
                        "Config schema_version {} is newer than supported version {}; entering read-only mode",
                        cfg.schema_version, CURRENT_SCHEMA_VERSION
                    );
                    log_warn("config", &msg);
                    self.warnings.push(msg);
                    self.read_only = true;
                    self.last_status = ConfigLoadStatus::LoadedReadOnlyFutureSchema {
                        found_version: cfg.schema_version,
                    };
                    return cfg;
                }
                if cfg.schema_version < CURRENT_SCHEMA_VERSION {
                    self.preserve_pre_migration_copy(cfg.schema_version);
                    cfg.schema_version = CURRENT_SCHEMA_VERSION;
                }
                if cfg.target.monitor_policy == MonitorPolicy::Fixed
                    && cfg.target.monitor_id.is_none()
                    && !cfg.bindings.iter().any(|b| b.enabled)
                {
                    cfg.target.monitor_policy = MonitorPolicy::AllMonitors;
                }
                self.last_status = ConfigLoadStatus::LoadedNormally;
                cfg
            }
            Err(primary_err) => {
                log_error(
                    "config",
                    &format!("Primary config.json corrupted: {}", primary_err),
                );
                self.quarantine_corrupted_file();

                let bak = self.backup_path();
                if bak.exists() {
                    match self.read_and_validate_file(&bak) {
                        Ok((cfg, file_warnings)) => {
                            log_warn("config", "Successfully recovered configuration from config.json.bak");
                            self.warnings.extend(file_warnings);
                            if cfg.schema_version > CURRENT_SCHEMA_VERSION {
                                self.read_only = true;
                            }
                            self.last_status = ConfigLoadStatus::RecoveredFromBackup {
                                error: primary_err,
                            };
                            if !self.read_only {
                                let _ = self.save(&cfg);
                            }
                            return cfg;
                        }
                        Err(bak_err) => {
                            log_error(
                                "config",
                                &format!("Backup config.json.bak also failed: {}", bak_err),
                            );
                        }
                    }
                }

                self.last_status = ConfigLoadStatus::ResetAfterCorruption {
                    error: primary_err,
                };
                AppConfig::default_initial()
            }
        }
    }

    /// Atomically save configuration to disk.
    /// Does nothing if `is_read_only()` is true (e.g. loaded from a newer schema version).
    pub fn save(&self, config: &AppConfig) -> Result<(), String> {
        if self.read_only {
            log_warn(
                "config",
                "Skipping config save because read-only mode is active (newer schema_version)",
            );
            return Err("Configuration is in read-only mode".to_string());
        }

        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {}", e))?;
        }

        let json_bytes = serde_json::to_vec_pretty(config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        let tmp_path = self.config_path.with_extension("json.tmp");
        {
            let mut file = File::create(&tmp_path)
                .map_err(|e| format!("Failed to create temporary config file: {}", e))?;
            file.write_all(&json_bytes)
                .map_err(|e| format!("Failed to write temporary config file: {}", e))?;
            // Calls FlushFileBuffers on Windows
            file.sync_all()
                .map_err(|e| format!("Failed to flush temporary config file: {}", e))?;
        }

        let bak_path = self.backup_path();
        atomic_replace_file(&tmp_path, &self.config_path, Some(&bak_path))?;
        Ok(())
    }

    fn read_and_validate_file(&self, path: &Path) -> Result<(AppConfig, Vec<String>), String> {
        let mut file =
            File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
        let mut raw = String::new();
        file.read_to_string(&mut raw)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

        // Strip UTF-8 BOM if a user edited the file in Notepad
        let content = raw.strip_prefix('\u{feff}').unwrap_or(&raw);

        let value: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| format!("Invalid JSON in {}: {}", path.display(), e))?;

        let obj = value
            .as_object()
            .ok_or_else(|| "Root of config.json must be a JSON object".to_string())?;

        if !obj.contains_key("schema_version") {
            return Err("Missing required field 'schema_version'".to_string());
        }
        let schema_ver = obj["schema_version"]
            .as_u64()
            .ok_or_else(|| "'schema_version' must be a positive integer".to_string())?;
        if schema_ver == 0 {
            return Err("'schema_version' must be >= 1".to_string());
        }

        let mut warnings = Vec::new();
        const KNOWN_TOP_KEYS: &[&str] = &[
            "schema_version",
            "launcher",
            "target",
            "scenes",
            "shortcuts",
            "bindings",
            "hotkeys",
            "layouts",
            "monitors",
        ];
        for key in obj.keys() {
            if !KNOWN_TOP_KEYS.contains(&key.as_str()) {
                let msg = format!("Unknown top-level config key '{}' (ignored)", key);
                log_warn("config", &msg);
                warnings.push(msg);
            }
        }

        let cfg: AppConfig = serde_json::from_value(value)
            .map_err(|e| format!("Schema validation error in {}: {}", path.display(), e))?;

        Ok((cfg, warnings))
    }

    fn preserve_pre_migration_copy(&self, old_version: u32) {
        let archive = self
            .config_path
            .with_file_name(format!("config.v{}.json", old_version));
        if let Err(e) = fs::copy(&self.config_path, &archive) {
            log_warn(
                "config",
                &format!("Failed to preserve pre-migration config: {}", e),
            );
        }
    }

    fn quarantine_corrupted_file(&self) {
        let ts = crate::diagnostics::current_unix_ms();
        let bad_path = self
            .config_path
            .with_file_name(format!("config.json.invalid-{}", ts));
        let _ = fs::copy(&self.config_path, &bad_path);
    }
}

/// Atomically replace `dest_path` with `tmp_path`, optionally saving the previous `dest_path` to `backup_path`.
/// Uses Win32 `ReplaceFileW` when `dest_path` already exists, and `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`
/// when creating `dest_path` for the first time.
#[cfg(windows)]
pub fn atomic_replace_file(
    tmp_path: &Path,
    dest_path: &Path,
    backup_path: Option<&Path>,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACE_FILE_FLAGS,
    };

    let to_wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    let tmp_w = to_wide(tmp_path);
    let dest_w = to_wide(dest_path);

    unsafe {
        if dest_path.exists() {
            let bak_w = backup_path.map(to_wide);
            let bak_ptr = match &bak_w {
                Some(v) => PCWSTR(v.as_ptr()),
                None => PCWSTR::null(),
            };

            let replace_res = ReplaceFileW(
                PCWSTR(dest_w.as_ptr()),
                PCWSTR(tmp_w.as_ptr()),
                bak_ptr,
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            );
            if replace_res.is_ok() {
                return Ok(());
            }
            // Fallback to MoveFileExW if ReplaceFileW failed (e.g. backup lock)
            if let Some(bak) = backup_path {
                let _ = fs::copy(dest_path, bak);
            }
        }

        MoveFileExW(
            PCWSTR(tmp_w.as_ptr()),
            PCWSTR(dest_w.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|e| format!("MoveFileExW failed: {}", e))
    }
}

#[cfg(not(windows))]
pub fn atomic_replace_file(
    tmp_path: &Path,
    dest_path: &Path,
    backup_path: Option<&Path>,
) -> Result<(), String> {
    if dest_path.exists() {
        if let Some(bak) = backup_path {
            let _ = fs::copy(dest_path, bak);
        }
    }
    fs::rename(tmp_path, dest_path).map_err(|e| format!("rename failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(suffix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "kiri_test_{}_{}_{}",
            std::process::id(),
            crate::diagnostics::current_unix_ms(),
            suffix
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn test_default_config_and_atomic_save_backup() {
        let dir = temp_test_dir("atomic");
        let cfg_path = dir.join("config.json");
        let mut mgr = ConfigManager::new(cfg_path.clone());

        let mut cfg = mgr.load();
        assert_eq!(*mgr.last_status(), ConfigLoadStatus::CreatedDefault);
        assert!(cfg_path.exists());

        // Modify and save again -> should create .bak via ReplaceFileW
        cfg.launcher.visible_slots = 4;
        mgr.save(&cfg).expect("second save should succeed");
        assert!(mgr.backup_path().exists());

        // Corrupt primary config.json and verify recovery from .bak
        fs::write(&cfg_path, b"{ broken json").unwrap();
        let recovered = mgr.load();
        assert!(matches!(
            mgr.last_status(),
            ConfigLoadStatus::RecoveredFromBackup { .. }
        ));
        assert_eq!(recovered.launcher.visible_slots, 3); // from backup before the second change

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_future_schema_version_enters_read_only_mode() {
        let dir = temp_test_dir("readonly");
        let cfg_path = dir.join("config.json");
        let mut future_cfg = AppConfig::default_initial();
        future_cfg.schema_version = CURRENT_SCHEMA_VERSION + 5;
        fs::write(&cfg_path, serde_json::to_vec_pretty(&future_cfg).unwrap()).unwrap();

        let mut mgr = ConfigManager::new(cfg_path);
        let loaded = mgr.load();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION + 5);
        assert!(mgr.is_read_only());
        assert!(mgr.save(&loaded).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shortcut_settings_round_trip_with_legacy_launcher_defaults() {
        let mut config: AppConfig = serde_json::from_str(
            r#"{"schema_version":1,"launcher":{"orientation":"vertical"}}"#,
        ).unwrap();
        assert!(config.launcher.show_bar);
        assert!(config.launcher.ctrl_double_tap);
        assert_eq!(
            config.launcher.cursor_popup_position,
            CursorPopupPosition::SceneCentered
        );
        assert_eq!(config.launcher.orientation, Orientation::Vertical);
        assert!(config.shortcuts.is_empty());

        config.launcher.show_bar = false;
        config.launcher.ctrl_double_tap = false;
        config.launcher.cursor_popup_position = CursorPopupPosition::BesideCursor;
        config.shortcuts = vec![Shortcut {
            name: "プロジェクト".to_owned(),
            kind: ShortcutKind::Folder,
            target: r"D:\作業\Kiri Project".to_owned(),
        }, Shortcut {
            name: "GitHub".to_owned(),
            kind: ShortcutKind::Website,
            target: "https://github.com/".to_owned(),
        }];
        #[cfg(windows)]
        assert!(config.shortcuts[0].validate().is_ok());
        assert!(config.shortcuts[1].validate().is_ok());
        let saved = serde_json::to_vec(&config).unwrap();
        assert_eq!(serde_json::from_slice::<AppConfig>(&saved).unwrap(), config);
    }

    #[test]
    fn test_unknown_keys_emit_warning_not_error() {
        let dir = temp_test_dir("unknown_keys");
        let cfg_path = dir.join("config.json");
        let raw = r#"{
            "schema_version": 1,
            "experimental_future_flag": true,
            "launcher": { "orientation": "horizontal" },
            "target": { "monitor_policy": "fixed", "monitor_id": null },
            "scenes": [],
            "bindings": []
        }"#;
        fs::write(&cfg_path, raw).unwrap();

        let mut mgr = ConfigManager::new(cfg_path);
        let _ = mgr.load();
        assert_eq!(*mgr.last_status(), ConfigLoadStatus::LoadedNormally);
        assert_eq!(mgr.warnings().len(), 1);
        assert!(mgr.warnings()[0].contains("experimental_future_flag"));

        let _ = fs::remove_dir_all(&dir);
    }
}
