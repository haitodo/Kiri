use super::{is_autostart_enabled, RegistrationWarning, SettingsViewModel};
use crate::app::AppController;
use crate::config::{
    AppConfig, CursorPopupPosition, LauncherConfig, MonitorPolicy, Orientation, Scene,
    SceneVariant, Shortcut, TargetConfig, TitleRule, TitleRuleType, WindowBinding,
};
use crate::diagnostics::executable_name;
use crate::launcher::paint::{is_builtin_icon, BUILTIN_ICONS};
use crate::platform::WindowInstance;
use crate::scenes::switch::{format_switch_warnings, preview_scene_switch};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Clone, Copy)]
pub enum SettingsPage {
    Scenes,
    Shortcuts,
}

impl SettingsPage {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Scenes => "scenes",
            Self::Shortcuts => "shortcuts",
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(super) enum Command {
    #[serde(skip_deserializing)]
    Prepare,
    Initialize,
    Rendered {
        dark: bool,
    },
    StartupFailed,
    Appearance {
        dark: bool,
    },
    Refresh,
    CreateScene {
        name: String,
    },
    EditScene {
        id: String,
        name: String,
        icon: String,
        pinned: bool,
        order: usize,
    },
    DeleteScene {
        id: String,
    },
    DeleteLayout {
        id: String,
    },
    SceneTarget {
        scene: String,
        policy: Option<MonitorPolicy>,
        monitor: Option<String>,
    },
    CreateVariant {
        scene: String,
        original_name: Option<String>,
        name: String,
    },
    RenameVariant {
        scene: String,
        variant: String,
        name: String,
    },
    DeleteVariant {
        scene: String,
        variant: String,
    },
    AddWindows {
        scene: String,
        variant: Option<String>,
        candidates: Vec<String>,
    },
    RemoveFromScene {
        scene: String,
        variant: Option<String>,
        id: String,
    },
    ShareInScene {
        scene: String,
        id: String,
    },
    EditBinding {
        id: String,
        scenes: Vec<String>,
        variants: BTreeMap<String, Vec<String>>,
        always: bool,
        enabled: bool,
        rule: TitleRule,
        monitor: Option<String>,
    },
    ChooseWindow {
        id: String,
        candidate: String,
    },
    DeleteBinding {
        id: String,
    },
    Launcher {
        values: LauncherOptions,
    },
    ResetPosition,
    Target {
        policy: MonitorPolicy,
        monitor: Option<String>,
    },
    Behavior {
        restore: bool,
        focus: bool,
        autostart: bool,
        verbose: bool,
    },
    SetHotkey {
        scene: String,
        modifiers: u32,
        key: Option<u32>,
    },
    EditShortcut {
        index: Option<usize>,
        order: usize,
        shortcut: Shortcut,
    },
    DeleteShortcut {
        index: usize,
    },
    Inspect {
        scene: String,
        variant: Option<String>,
    },
    Save {
        tray_only_confirmed: bool,
    },
    Discard,
    Import,
    Export,
    #[serde(skip_deserializing)]
    ImportPath(String),
    #[serde(skip_deserializing)]
    ExportPath(String),
    OpenFolder,
    Browse {
        kind: FileKind,
    },
    RecoveryPreview,
    Restore {
        candidates: Vec<String>,
    },
    Close {
        discard: bool,
    },
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(super) enum FileKind {
    Image,
    Folder,
    File,
}

#[derive(Deserialize)]
pub(super) struct LauncherOptions {
    show_bar: bool,
    ctrl_double_tap: bool,
    cursor_popup_position: CursorPopupPosition,
    orientation: Orientation,
    button_size_dip: i32,
    grip_size_dip: i32,
    visible_slots: usize,
    always_on_top: bool,
    snap_to_edges: bool,
    wheel_wrap: bool,
}

impl LauncherOptions {
    fn apply(self, launcher: &mut LauncherConfig) -> Result<(), String> {
        if !(20..=96).contains(&self.button_size_dip)
            || !(8..=48).contains(&self.grip_size_dip)
            || !(1..=12).contains(&self.visible_slots)
        {
            return Err(
                "ボタンは 20〜96、つまみは 8〜48、表示件数は 1〜12 の範囲で入力してください。"
                    .into(),
            );
        }
        launcher.show_bar = self.show_bar;
        launcher.ctrl_double_tap = self.ctrl_double_tap;
        launcher.cursor_popup_position = self.cursor_popup_position;
        launcher.orientation = self.orientation;
        launcher.button_size_dip = self.button_size_dip;
        launcher.grip_size_dip = self.grip_size_dip;
        launcher.visible_slots = self.visible_slots;
        launcher.always_on_top = self.always_on_top;
        launcher.snap_to_edges = self.snap_to_edges;
        launcher.wheel_wrap = self.wheel_wrap;
        Ok(())
    }
}

pub(super) struct Editor {
    pub(super) vm: SettingsViewModel,
    baseline: AppConfig,
    baseline_autostart: bool,
    pub(super) autostart: bool,
    imported: bool,
    reset_position: bool,
    deleted_layouts: HashSet<String>,
    detached: HashSet<(String, String)>,
    pub(super) read_only: bool,
}

impl Editor {
    pub(super) fn new(app: &AppController) -> Self {
        let autostart = is_autostart_enabled();
        Self {
            vm: SettingsViewModel::build(&app.config, &*app.sys, launcher_monitor(app)),
            baseline: app.config.clone(),
            baseline_autostart: autostart,
            autostart,
            imported: false,
            reset_position: false,
            deleted_layouts: HashSet::new(),
            detached: HashSet::new(),
            read_only: app.config_mgr.is_read_only(),
        }
    }

    pub(super) fn dirty(&self) -> bool {
        self.reset_position
            || !self.deleted_layouts.is_empty()
            || self.vm.config_draft != self.baseline
            || self.autostart != self.baseline_autostart
    }

    pub(super) fn refresh(&mut self, app: &AppController) {
        self.vm.set_launcher_monitor(launcher_monitor(app));
        self.vm.refresh_candidates(&*app.sys);
    }

    pub(super) fn reset(&mut self, app: &AppController) {
        *self = Self::new(app);
    }

    pub(super) fn import(&mut self, config: AppConfig, app: &AppController) {
        self.vm.config_draft = config;
        self.deleted_layouts.clear();
        self.detached.clear();
        self.reset_position = false;
        self.imported = true;
        self.refresh(app);
    }

    pub(super) fn config_to_save(&self, live: &AppConfig) -> AppConfig {
        let mut config = self.vm.config_draft.clone();
        if !self.imported {
            // The launcher can move, switch scenes, or save layouts while settings are open.
            if !self.reset_position {
                config.launcher.position = live.launcher.position.clone();
            }
            if config.launcher.orientation == self.baseline.launcher.orientation {
                config.launcher.orientation = live.launcher.orientation;
            }
            config.monitors = live.monitors.clone();
            for scene in &mut config.scenes {
                if let Some(id) = live
                    .scene(&scene.id)
                    .and_then(|current| current.last_variant_id.as_ref())
                {
                    if scene.variants.iter().any(|variant| &variant.id == id) {
                        scene.last_variant_id = Some(id.clone());
                    }
                }
            }
            for profile in &mut config.monitors {
                if profile
                    .last_active_scene_id
                    .as_ref()
                    .is_some_and(|id| !config.scenes.iter().any(|scene| &scene.id == id))
                {
                    profile.last_active_scene_id = None;
                }
            }
            config.layouts = live
                .layouts
                .iter()
                .filter(|layout| {
                    !self.deleted_layouts.contains(&layout.scene_id)
                        && config.scenes.iter().any(|s| s.id == layout.scene_id)
                })
                .cloned()
                .map(|mut layout| {
                    layout.entries.retain(|entry| {
                        config.bindings.iter().any(|b| {
                            b.id == entry.binding_id && b.belongs_to_scene(&layout.scene_id)
                        }) && !self
                            .detached
                            .contains(&(layout.scene_id.clone(), entry.binding_id.clone()))
                    });
                    layout
                })
                .collect();
        }
        config
    }

    pub(super) fn edit(&mut self, command: Command, app: &AppController) -> Result<Value, String> {
        if self.read_only {
            return Err(
                "この設定は読み取り専用です。対応する新しいバージョンの Kiri で編集してください。"
                    .into(),
            );
        }
        let mut extra = Value::Null;
        use Command::*;
        match command {
            CreateScene { name } => {
                let config = &mut self.vm.config_draft;
                let name = scene_name(config, None, name)?;
                let number = (1..)
                    .find(|n| {
                        let id = format!("scene-{n}");
                        !config.scenes.iter().any(|s| s.id == id)
                            && !self.baseline.scenes.iter().any(|s| s.id == id)
                            && !config.bindings.iter().any(|b| b.belongs_to_scene(&id))
                            && !config.hotkeys.iter().any(|h| h.scene_id == id)
                            && !config.layouts.iter().any(|l| l.scene_id == id)
                    })
                    .unwrap();
                let id = format!("scene-{number}");
                let mut order = config.scenes.iter().map(|s| s.order).max().unwrap_or(0);
                if order == i32::MAX {
                    normalize_scene_order(config);
                    order = config.scenes.len() as i32;
                }
                config.scenes.push(Scene {
                    id: id.clone(),
                    name,
                    icon: "star".into(),
                    order: order + 1,
                    pinned: true,
                    ..Scene::default()
                });
                extra = json!({ "scene": id });
            }
            EditScene {
                id,
                name,
                icon,
                pinned,
                order,
            } => {
                let config = &mut self.vm.config_draft;
                let name = scene_name(config, Some(&id), name)?;
                let index = config
                    .ordered_scenes()
                    .iter()
                    .position(|s| s.id == id)
                    .ok_or("シーンが見つかりません。")?;
                if order >= config.scenes.len() {
                    return Err("表示の順番を選んでください。".into());
                }
                let scene = config
                    .scenes
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or("シーンが見つかりません。")?;
                if icon != scene.icon && !is_builtin_icon(&icon) && !supported_image(&icon) {
                    return Err(
                        "アイコンはプリセット、または ICO・PNG・BMP ファイルを選んでください。"
                            .into(),
                    );
                }
                scene.name = name;
                scene.icon = icon;
                scene.pinned = pinned;
                if order != index {
                    normalize_scene_order(config);
                    let scene = config.scenes.remove(index);
                    config.scenes.insert(order, scene);
                    for (idx, scene) in config.scenes.iter_mut().enumerate() {
                        scene.order = idx as i32 + 1;
                    }
                }
            }
            DeleteScene { id } => {
                let config = &mut self.vm.config_draft;
                require_scene(config, &id)?;
                config.scenes.retain(|s| s.id != id);
                self.deleted_layouts.insert(id.clone());
                config.hotkeys.retain(|h| h.scene_id != id);
                config.layouts.retain(|l| l.scene_id != id);
                config.bindings.retain_mut(|binding| {
                    let was_member = binding.belongs_to_scene(&id);
                    binding.remove_from_scene(&id, None);
                    binding.scene_variants.remove(&id);
                    !was_member || binding.always_visible || binding.has_scene()
                });
                self.vm.refresh_annotations();
            }
            DeleteLayout { id } => {
                require_scene(&self.vm.config_draft, &id)?;
                self.vm.delete_scene_layouts(&id);
                self.deleted_layouts.insert(id);
            }
            SceneTarget {
                scene,
                policy,
                monitor,
            } => {
                self.validate_fixed_target(policy, monitor.as_deref())?;
                let scene = require_scene_mut(&mut self.vm.config_draft, &scene)?;
                scene.target = policy.map(|monitor_policy| TargetConfig {
                    monitor_policy,
                    monitor_id: monitor,
                });
            }
            CreateVariant {
                scene,
                original_name,
                name,
            } => {
                let config = &mut self.vm.config_draft;
                let saved = self.baseline.scene(&scene);
                let item = require_scene_mut(config, &scene)?;
                let name = variant_name(item, None, name)?;
                if item.variants.is_empty() {
                    let original_name =
                        variant_name(item, None, original_name.unwrap_or_default())?;
                    if original_name.eq_ignore_ascii_case(&name) {
                        return Err("構成には別々の名前を付けてください。".into());
                    }
                    let id = next_variant_id(item, saved);
                    item.variants.push(SceneVariant {
                        id: id.clone(),
                        name: original_name,
                    });
                    for binding in &mut config.bindings {
                        if binding.belongs_to_selection(&scene, None) {
                            binding.remove_from_scene(&scene, None);
                            binding.add_to_scene(&scene, Some(&id));
                        }
                    }
                }
                let item = require_scene_mut(config, &scene)?;
                let id = next_variant_id(item, saved);
                item.variants.push(SceneVariant {
                    id: id.clone(),
                    name,
                });
                self.vm.refresh_annotations();
                extra = json!({ "variant": id });
            }
            RenameVariant {
                scene,
                variant,
                name,
            } => {
                let item = require_scene_mut(&mut self.vm.config_draft, &scene)?;
                let name = variant_name(item, Some(&variant), name)?;
                item.variants
                    .iter_mut()
                    .find(|v| v.id == variant)
                    .ok_or("構成が見つかりません。")?
                    .name = name;
            }
            DeleteVariant { scene, variant } => self.delete_variant(&scene, &variant)?,
            AddWindows {
                scene,
                variant,
                candidates,
            } => {
                let item = require_selection(&self.vm.config_draft, &scene, variant.as_deref())?;
                let target = item.target(&self.vm.config_draft.target).clone();
                let windows = self.live_candidates(&candidates, app)?;
                if target.monitor_policy != MonitorPolicy::AllMonitors {
                    let monitor = self.vm.target_monitor(&target);
                    if windows
                        .iter()
                        .any(|win| Some(win.monitor_id.as_str()) != monitor.as_deref())
                    {
                        return Err("切り替える画面にあるウィンドウを選んでください。".into());
                    }
                }
                if windows.iter().any(|win| self.ambiguous_candidate(win.hwnd)) {
                    return Err("一致する登録済みウィンドウがあります。「登録済みウィンドウを管理」から対象を確認してください。".into());
                }
                for win in windows {
                    self.vm
                        .add_window_to_scene(&win, &scene, variant.as_deref());
                }
            }
            RemoveFromScene { scene, variant, id } => {
                require_selection(&self.vm.config_draft, &scene, variant.as_deref())?;
                self.vm
                    .unbind_window_from_scene(&id, &scene, variant.as_deref());
                if !self
                    .vm
                    .config_draft
                    .bindings
                    .iter()
                    .any(|binding| binding.id == id && binding.belongs_to_scene(&scene))
                {
                    self.detached.insert((scene, id));
                }
            }
            ShareInScene { scene, id } => {
                require_scene(&self.vm.config_draft, &scene)?;
                let binding = self
                    .vm
                    .config_draft
                    .bindings
                    .iter_mut()
                    .find(|binding| binding.id == id && binding.belongs_to_scene(&scene))
                    .ok_or("このシーンにウィンドウが登録されていません。")?;
                binding.add_to_scene(&scene, None);
                self.vm.refresh_annotations();
            }
            EditBinding {
                id,
                scenes,
                variants,
                always,
                enabled,
                rule,
                monitor,
            } => {
                self.edit_binding(&id, scenes, variants, always, enabled, rule, monitor)?;
            }
            ChooseWindow { id, candidate } => {
                let windows = self.live_candidates(&[candidate], app)?;
                let win = windows.first().ok_or("ウィンドウを選んでください。")?;
                let row = self
                    .vm
                    .binding_rows
                    .iter()
                    .find(|row| row.binding_id == id)
                    .ok_or("登録が見つかりません。")?;
                if !row
                    .ambiguous_candidates
                    .iter()
                    .any(|candidate| candidate.hwnd == win.hwnd)
                {
                    return Err(
                        "このウィンドウは現在の候補ではありません。登録条件を確認してください。"
                            .into(),
                    );
                }
                self.vm.resolve_binding_to_instance(&id, win);
            }
            DeleteBinding { id } => self.vm.remove_binding(&id),
            Launcher { values } => values.apply(&mut self.vm.config_draft.launcher)?,
            ResetPosition => {
                self.vm.config_draft.launcher.position = None;
                self.reset_position = true;
            }
            Target { policy, monitor } => {
                self.validate_fixed_target(Some(policy), monitor.as_deref())?;
                let target = &mut self.vm.config_draft.target;
                target.monitor_policy = policy;
                if policy == MonitorPolicy::Fixed {
                    target.monitor_id = monitor;
                }
                self.vm.refresh_annotations();
            }
            Behavior {
                restore,
                focus,
                autostart,
                verbose,
            } => {
                self.vm.config_draft.launcher.restore_all_on_switch = restore;
                self.vm.config_draft.launcher.focus_on_switch = focus;
                self.autostart = autostart;
                self.vm.config_draft.verbose_title_logging = verbose;
            }
            SetHotkey {
                scene,
                modifiers,
                key,
            } => {
                require_scene(&self.vm.config_draft, &scene)?;
                if let Some(key) = key {
                    if !(1..=254).contains(&key) || modifiers > 15 {
                        return Err("有効なキーの組み合わせを選んでください。".into());
                    }
                    if let Some(other) =
                        self.vm.config_draft.hotkeys.iter().find(|h| {
                            h.scene_id != scene && h.vk == key && h.modifiers == modifiers
                        })
                    {
                        let name = self
                            .vm
                            .config_draft
                            .scenes
                            .iter()
                            .find(|s| s.id == other.scene_id)
                            .map_or(other.scene_id.as_str(), |s| s.name.as_str());
                        return Err(format!("このキーは「{name}」に設定されています。"));
                    }
                }
                if self.vm.get_scene_hotkey(&scene) != key.map(|key| (modifiers, key)) {
                    self.vm.set_scene_hotkey(&scene, modifiers, key);
                }
            }
            EditShortcut {
                index,
                order,
                mut shortcut,
            } => {
                shortcut.name = shortcut.name.trim().to_owned();
                shortcut.target = shortcut.target.trim().to_owned();
                shortcut.validate()?;
                let shortcuts = &mut self.vm.config_draft.shortcuts;
                match index {
                    Some(index) if index < shortcuts.len() && order < shortcuts.len() => {
                        shortcuts.remove(index);
                        shortcuts.insert(order, shortcut);
                    }
                    Some(_) => {
                        return Err("ショートカットまたは表示の順番が見つかりません。".into())
                    }
                    None => shortcuts.push(shortcut),
                };
            }
            DeleteShortcut { index } => {
                let list = &mut self.vm.config_draft.shortcuts;
                if index >= list.len() {
                    return Err("ショートカットが見つかりません。".into());
                }
                list.remove(index);
            }
            _ => return Err("設定の操作を処理できませんでした。".into()),
        }
        Ok(extra)
    }

    fn validate_fixed_target(
        &self,
        policy: Option<MonitorPolicy>,
        monitor: Option<&str>,
    ) -> Result<(), String> {
        if policy == Some(MonitorPolicy::Fixed)
            && !self
                .vm
                .monitors
                .iter()
                .any(|mon| Some(mon.monitor_id.as_str()) == monitor)
        {
            return Err("接続されている画面を選んでください。".into());
        }
        Ok(())
    }

    fn delete_variant(&mut self, scene: &str, variant: &str) -> Result<(), String> {
        let config = &mut self.vm.config_draft;
        let item = require_scene_mut(config, scene)?;
        let index = item
            .variants
            .iter()
            .position(|v| v.id == variant)
            .ok_or("構成が見つかりません。")?;
        item.variants.remove(index);
        if item.last_variant_id.as_deref() == Some(variant) {
            item.last_variant_id = None;
        }
        let remaining = if item.variants.len() == 1 {
            Some(item.variants.remove(0).id)
        } else {
            None
        };
        if item.variants.is_empty() {
            item.last_variant_id = None;
        }
        config.bindings.retain_mut(|binding| {
            let was_member = binding.belongs_to_scene(scene);
            binding.remove_from_scene(scene, Some(variant));
            if let Some(id) = &remaining {
                if binding.belongs_to_selection(scene, Some(id)) {
                    binding.add_to_scene(scene, None);
                }
            }
            if was_member && !binding.belongs_to_scene(scene) {
                self.detached.insert((scene.to_owned(), binding.id.clone()));
                return binding.always_visible || binding.has_scene();
            }
            true
        });
        for layout in config
            .layouts
            .iter_mut()
            .filter(|layout| layout.scene_id == scene)
        {
            layout.entries.retain(|entry| {
                config.bindings.iter().any(|binding| {
                    binding.id == entry.binding_id && binding.belongs_to_scene(scene)
                })
            });
        }
        self.vm.refresh_annotations();
        Ok(())
    }

    /// Returns a warning if only the separate Windows logon preference could not be saved.
    pub(super) fn save(
        &mut self,
        app: &mut AppController,
        tray_only_confirmed: bool,
    ) -> Result<(bool, Option<String>), String> {
        if self.read_only {
            return Err("この設定は読み取り専用です。".into());
        }
        let config = self.config_to_save(&app.config);
        if !config.launcher.show_bar && !config.launcher.ctrl_double_tap && !tray_only_confirmed {
            return Err(
                "固定バーと Ctrl の二度押しを両方無効にする場合は、確認してください。".into(),
            );
        }
        let changed = config != app.config;
        if changed {
            app.save_user_settings(config)
                .map_err(|e| format!("設定を保存できませんでした。{e}"))?;
        }
        self.baseline = app.config.clone();
        self.vm.config_draft = app.config.clone();
        self.imported = false;
        self.reset_position = false;
        self.deleted_layouts.clear();
        self.detached.clear();

        let warning = if self.autostart != self.baseline_autostart {
            match super::set_autostart_enabled(self.autostart) {
                Ok(()) => {
                    self.baseline_autostart = self.autostart;
                    None
                }
                Err(e) => Some(format!(
                    "Windows への自動起動の変更を保存できませんでした。{e}"
                )),
            }
        } else {
            None
        };
        Ok((changed, warning))
    }

    fn live_candidates(
        &mut self,
        keys: &[String],
        app: &AppController,
    ) -> Result<Vec<WindowInstance>, String> {
        let previous: HashSet<_> = self
            .vm
            .candidate_windows
            .iter()
            .map(|row| candidate_key(&row.instance))
            .collect();
        self.refresh(app);
        let mut seen = HashSet::new();
        keys.iter().filter(|key| seen.insert(*key)).map(|key| {
            let candidate = self.vm.candidate_windows.iter().find(|row| {
                previous.contains(key) && candidate_key(&row.instance) == *key
            }).ok_or("選んだウィンドウが移動したか、閉じられました。一覧を更新して選び直してください。")?;
            if let Some(reason) = candidate_block_reason(&candidate.instance) {
                return Err(reason.into());
            }
            Ok(candidate.instance.clone())
        }).collect()
    }

    fn ambiguous_candidate(&self, hwnd: u64) -> bool {
        self.vm.binding_rows.iter().any(|binding| {
            binding
                .ambiguous_candidates
                .iter()
                .any(|win| win.hwnd == hwnd)
        })
    }

    fn edit_binding(
        &mut self,
        id: &str,
        scenes: Vec<String>,
        mut variants: BTreeMap<String, Vec<String>>,
        always: bool,
        enabled: bool,
        mut rule: TitleRule,
        monitor: Option<String>,
    ) -> Result<(), String> {
        let config = &mut self.vm.config_draft;
        for id in &scenes {
            require_scene(config, id)?;
        }
        variants.retain(|scene, ids| !ids.is_empty() && !scenes.contains(scene));
        for (scene, ids) in &variants {
            for variant in ids {
                require_selection(config, scene, Some(variant))?;
            }
        }
        let binding = config
            .bindings
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or("登録が見つかりません。")?;
        if rule != binding.match_rule.title_rule {
            if rule.rule_type == TitleRuleType::Any {
                rule.value = None;
            } else {
                let value = rule.value.as_deref().unwrap_or_default().trim();
                if value.is_empty() {
                    return Err("ウィンドウ名の条件を入力してください。".into());
                }
                if rule.rule_type == TitleRuleType::Regex && regex::Regex::new(value).is_err() {
                    return Err("正規表現を確認してください。".into());
                }
                rule.value = Some(value.into());
            }
        }
        if monitor != binding.match_rule.monitor_id
            && monitor
                .as_ref()
                .is_some_and(|id| !self.vm.monitors.iter().any(|m| &m.monitor_id == id))
        {
            return Err("接続されている画面を選んでください。".into());
        }
        if scenes.is_empty() && variants.is_empty() && enabled && !always {
            return Err("シーンを選ぶか、この登録を無効にしてください。".into());
        }
        if rule != binding.match_rule.title_rule || monitor != binding.match_rule.monitor_id {
            binding.instance_hint = None;
        }
        for scene in binding
            .scene_ids
            .iter()
            .chain(binding.scene_variants.keys())
        {
            if !scenes.contains(scene) && !variants.contains_key(scene) {
                self.detached.insert((scene.clone(), id.into()));
            }
        }
        binding.match_rule.title_rule = rule;
        binding.match_rule.monitor_id = monitor;
        binding.always_visible = always;
        binding.enabled = enabled;
        binding.scene_ids = scenes;
        binding.scene_variants = variants;
        for layout in config
            .layouts
            .iter_mut()
            .filter(|l| !binding.belongs_to_scene(&l.scene_id))
        {
            layout.entries.retain(|entry| entry.binding_id != id);
        }
        self.vm.refresh_annotations();
        Ok(())
    }

    pub(super) fn inspect(&self, scene: &str, variant: Option<&str>, app: &AppController) -> Value {
        let config = &self.vm.config_draft;
        let mut resolver = app.resolver.clone();
        for binding in &config.bindings {
            if !app.config.bindings.iter().any(|old| {
                old.id == binding.id
                    && old.match_rule == binding.match_rule
                    && old.instance_hint == binding.instance_hint
                    && old.enabled == binding.enabled
            }) {
                resolver.clear_binding(&binding.id);
            }
        }
        let preview = preview_scene_switch(
            scene,
            variant,
            config,
            resolver,
            &app.recovery,
            &*app.sys,
            launcher_monitor(app),
        );
        let window = |win: &WindowInstance| {
            json!({
                "app": app_label(win), "title": win.title,
                "monitor": self.monitor_label(&win.monitor_id),
            })
        };
        json!({
            "restore": preview.restore.iter().map(window).collect::<Vec<_>>(),
            "minimize": preview.minimize.iter().map(window).collect::<Vec<_>>(),
            "keep": preview.unchanged.iter().map(|(win, reason)| json!({ "window": window(win), "reason": reason })).collect::<Vec<_>>(),
            "warnings": format_switch_warnings(&preview.warnings, &config.bindings, &config.scenes),
        })
    }

    fn monitor_label(&self, id: &str) -> String {
        self.vm
            .monitors
            .iter()
            .position(|m| m.monitor_id == id)
            .map_or_else(
                || "未接続の画面".into(),
                |index| format!("画面 {}", index + 1),
            )
    }

    pub(super) fn state(&self) -> Value {
        let config = &self.vm.config_draft;
        let (visible, overflow) = config.split_visible_and_overflow();
        let monitors: Vec<_> = self
            .vm
            .monitors
            .iter()
            .enumerate()
            .map(|(i, mon)| {
                json!({
                    "id": mon.monitor_id, "number": i + 1, "name": mon.friendly_name,
                    "rect": mon.monitor_rect, "primary": mon.is_primary,
                })
            })
            .collect();
        let candidates: Vec<_> = self
            .vm
            .candidate_windows
            .iter()
            .map(|row| {
                json!({
                    "key": candidate_key(&row.instance),
                    "app": app_label(&row.instance),
                    "title": row.instance.title,
                    "monitor": self.monitor_label(&row.instance.monitor_id),
                    "monitor_id": row.instance.monitor_id,
                    "binding_id": row.existing_binding_id,
                    "blocked": candidate_block_reason(&row.instance),
                    "ambiguous": self.ambiguous_candidate(row.instance.hwnd),
                    "warnings": row.warnings.iter().filter_map(warning_label).collect::<Vec<_>>(),
                    "scenes": row.bound_scene_ids,
                })
            })
            .collect();
        let bindings: Vec<_> = config.bindings.iter().zip(&self.vm.binding_rows).filter(|(b, _)| {
            // The disabled install example is not a real user window.
            !(b.id == "example-mt5" && !b.enabled)
        }).map(|(binding, row)| {
            let live = self.vm.candidate_windows.iter().find(|candidate| Some(candidate.instance.hwnd) == row.live_hwnd);
            let monitor = live.map(|w| w.instance.monitor_id.as_str())
                .or(binding.match_rule.monitor_id.as_deref());
            json!({
                "id": binding.id, "app": live.map_or_else(|| binding_label(binding), |w| app_label(&w.instance)),
                "title": if row.is_running { row.window_title.as_str() } else { "" },
                "monitor_label": monitor.map(|id| self.monitor_label(id)),
                "monitor_id": monitor, "scenes": row.bound_scene_ids,
                "common_scenes": binding.scene_ids, "variants": binding.scene_variants,
                "always": binding.always_visible, "enabled": binding.enabled,
                "running": row.is_running, "warnings": row.warnings.iter().filter_map(warning_label).collect::<Vec<_>>(),
                "ambiguous": row.ambiguous_candidates.iter().map(candidate_key).collect::<Vec<_>>(),
                "rule": binding.match_rule.title_rule, "monitor": binding.match_rule.monitor_id,
            })
        }).collect();
        let scenes: Vec<_> = config.ordered_scenes().iter().map(|scene| {
            let (layouts, _) = self.vm.scene_layout_summary(&scene.id);
            json!({ "id": scene.id, "name": scene.name, "icon": scene.icon,
                "pinned": scene.pinned, "layouts": layouts,
                "variants": scene.variants, "preferred_variant": scene.preferred_variant().map(|v| &v.id),
                "target": scene.target, "effective_monitor": self.vm.target_monitor(scene.target(&config.target)),
                "hotkey": self.vm.get_scene_hotkey(&scene.id).map(|(mods, key)| json!({"mods": mods, "key": key})),
            })
        }).collect();
        json!({
            "scenes": scenes, "bindings": bindings, "candidates": candidates,
            "monitors": monitors, "target": config.target, "effective_monitor": self.vm.target_monitor(&config.target),
            "launcher": config.launcher, "shortcuts": config.shortcuts,
            "autostart": self.autostart, "verbose": config.verbose_title_logging,
            "read_only": self.read_only, "dirty": self.dirty(),
            "icons": BUILTIN_ICONS, "bar_scenes": visible, "overflow": !overflow.is_empty(),
        })
    }
}

fn normalize_scene_order(config: &mut AppConfig) {
    config
        .scenes
        .sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    for (i, scene) in config.scenes.iter_mut().enumerate() {
        scene.order = i as i32 + 1;
    }
}

fn candidate_block_reason(win: &WindowInstance) -> Option<&'static str> {
    if win.exe_path.is_empty() && win.aumid.is_none() {
        Some("アプリを識別できません")
    } else if win.is_elevated {
        Some("管理者権限で実行中")
    } else {
        None
    }
}

fn require_scene<'a>(config: &'a AppConfig, id: &str) -> Result<&'a Scene, String> {
    config
        .scene(id)
        .ok_or_else(|| "シーンが見つかりません。".into())
}

fn require_scene_mut<'a>(config: &'a mut AppConfig, id: &str) -> Result<&'a mut Scene, String> {
    config
        .scenes
        .iter_mut()
        .find(|scene| scene.id == id)
        .ok_or_else(|| "シーンが見つかりません。".into())
}

fn require_selection<'a>(
    config: &'a AppConfig,
    scene: &str,
    variant: Option<&str>,
) -> Result<&'a Scene, String> {
    let scene = require_scene(config, scene)?;
    if variant.is_some_and(|id| !scene.variants.iter().any(|variant| variant.id == id)) {
        return Err("構成が見つかりません。".into());
    }
    Ok(scene)
}

fn variant_name(scene: &Scene, current: Option<&str>, name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err("構成名を入力してください。".into());
    }
    if scene.variants.iter().any(|variant| {
        Some(variant.id.as_str()) != current && variant.name.eq_ignore_ascii_case(name)
    }) {
        return Err("同じ名前の構成があります。".into());
    }
    Ok(name.to_owned())
}

fn next_variant_id(scene: &Scene, saved: Option<&Scene>) -> String {
    let used = scene
        .variants
        .iter()
        .chain(saved.into_iter().flat_map(|scene| &scene.variants));
    (1..)
        .map(|n| format!("variant-{n}"))
        .find(|id| !used.clone().any(|variant| &variant.id == id))
        .unwrap()
}

fn scene_name(config: &AppConfig, id: Option<&str>, name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err("シーン名を入力してください。".into());
    }
    if config
        .scenes
        .iter()
        .any(|s| s.name == name && Some(s.id.as_str()) != id)
    {
        return Err("同じ名前のシーンがあります。".into());
    }
    Ok(name.into())
}

fn supported_image(path: &str) -> bool {
    Path::new(path).is_absolute()
        && Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ["ico", "png", "bmp"]
                    .iter()
                    .any(|kind| ext.eq_ignore_ascii_case(kind))
            })
}

pub(super) fn launcher_monitor(app: &AppController) -> Option<&str> {
    (!app.launcher_monitor_id.is_empty()).then_some(app.launcher_monitor_id.as_str())
}

pub(super) fn candidate_key(win: &WindowInstance) -> String {
    format!(
        "{}:{}:{}:{}",
        win.hwnd, win.pid, win.process_start_time, win.monitor_id
    )
}

fn friendly_app(path: &str, aumid: Option<&str>, class: &str) -> String {
    let exe = executable_name(path).unwrap_or_default();
    match exe.to_ascii_lowercase().as_str() {
        "code.exe" => "Visual Studio Code",
        "explorer.exe" => "エクスプローラー",
        "msedge.exe" => "Microsoft Edge",
        "chrome.exe" => "Google Chrome",
        "firefox.exe" => "Firefox",
        "windowsterminal.exe" => "Windows Terminal",
        "terminal64.exe" => "MetaTrader",
        "applicationframehost.exe" | "" => {
            return aumid
                .filter(|name| !name.is_empty())
                .unwrap_or(if class.is_empty() {
                    "ウィンドウ"
                } else {
                    class
                })
                .to_owned()
        }
        _ => return exe.trim_end_matches(".exe").to_owned(),
    }
    .to_owned()
}

pub(super) fn app_label(win: &WindowInstance) -> String {
    friendly_app(&win.exe_path, win.aumid.as_deref(), &win.window_class)
}

fn binding_label(binding: &WindowBinding) -> String {
    let rule = &binding.match_rule;
    friendly_app(&rule.exe_path, rule.aumid.as_deref(), &rule.window_class)
}

fn warning_label(warning: &RegistrationWarning) -> Option<String> {
    Some(match warning {
        RegistrationWarning::NoTaskbarButton => "タスクバーに表示されないウィンドウです".into(),
        RegistrationWarning::ElevatedProcess => "管理者権限で実行中".into(),
        RegistrationWarning::TitleMayContainSensitiveOrDynamicText => {
            "ウィンドウ名が変わることがあります".into()
        }
        RegistrationWarning::AmbiguousSameMonitor { count }
        | RegistrationWarning::AmbiguousCrossMonitor { count } => {
            format!("一致する候補が {count} 件あります")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigManager, LayoutSnapshot, SavedPosition};
    use crate::platform::fake::FakeWindowSystem;
    use crate::recovery::RecoveryManager;
    use std::{fs, path::PathBuf};

    fn sample_app(name: &str) -> (AppController, PathBuf) {
        let folder = std::env::temp_dir().join(format!(
            "kiri-settings-{}-{}-{name}",
            std::process::id(),
            crate::diagnostics::current_unix_ms()
        ));
        let app = AppController::new(
            ConfigManager::new(folder.join("config.json")),
            RecoveryManager::new(folder.join("runtime-state.json")),
            Box::new(FakeWindowSystem::new_dual_monitor()),
        );
        (app, folder)
    }

    #[test]
    fn failed_save_keeps_the_draft_and_leaves_the_running_app_unchanged() {
        let (mut app, folder) = sample_app("failed-save");
        let mut editor = Editor::new(&app);
        let original = app.config.clone();
        editor
            .edit(
                Command::CreateScene {
                    name: "開発の準備".into(),
                },
                &app,
            )
            .unwrap();
        let invalid_manager = ConfigManager::new(folder.join("config.json").join("blocked.json"));
        let original_manager = std::mem::replace(&mut app.config_mgr, invalid_manager);
        assert!(editor.save(&mut app, false).is_err());
        assert_eq!(app.config, original);
        assert!(editor.dirty());
        app.config_mgr = original_manager;
        assert_eq!(editor.save(&mut app, false).unwrap(), (true, None));
        assert!(!editor.dirty());
        assert_eq!(app.config_mgr.load(), app.config);
        assert!(app.config.scenes.iter().any(|s| s.name == "開発の準備"));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn unrelated_edits_keep_live_layouts_position_and_existing_monitor_policy() {
        let (mut app, folder) = sample_app("concurrent");
        app.config.target.monitor_policy = MonitorPolicy::LauncherMonitor;
        app.config.target.monitor_id = Some("temporarily-disconnected".into());
        app.config.launcher.restore_all_on_switch = false;
        let mut editor = Editor::new(&app);
        assert!(!editor.dirty());
        editor
            .edit(
                Command::CreateScene {
                    name: "会議".into(),
                },
                &app,
            )
            .unwrap();
        app.config.launcher.position = Some(SavedPosition {
            monitor_id: "MON-2-TARGET".into(),
            rel_x: 9.0,
            rel_y: 15.0,
            dpi: 96,
        });
        let layout = LayoutSnapshot {
            scene_id: "trade".into(),
            monitor_id: "MON-2-TARGET".into(),
            dpi: 120,
            entries: Vec::new(),
        };
        app.config.layouts.push(layout.clone());
        let pending = editor.config_to_save(&app.config);
        assert_eq!(pending.target, app.config.target);
        assert_eq!(pending.launcher.position, app.config.launcher.position);
        assert!(!pending.launcher.restore_all_on_switch);
        assert_eq!(pending.layouts, [layout]);
        editor.edit(Command::ResetPosition, &app).unwrap();
        editor
            .edit(Command::DeleteLayout { id: "trade".into() }, &app)
            .unwrap();
        let pending = editor.config_to_save(&app.config);
        assert!(pending.launcher.position.is_none());
        assert!(pending.layouts.is_empty());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn splitting_and_removing_variants_preserves_memberships_and_live_choice() {
        let (mut app, folder) = sample_app("variants");
        app.config.bindings[0].scene_ids.push("code".into());
        let mut editor = Editor::new(&app);
        let scene = "code".to_owned();
        let new_variant = editor
            .edit(
                Command::CreateVariant {
                    scene: scene.clone(),
                    original_name: Some("Antigravity".into()),
                    name: "Codex".into(),
                },
                &app,
            )
            .unwrap();
        let codex = new_variant["variant"].as_str().unwrap().to_owned();
        let draft = &editor.vm.config_draft;
        let original = draft
            .scene(&scene)
            .unwrap()
            .preferred_variant()
            .unwrap()
            .id
            .clone();
        let binding = &draft.bindings[0];
        assert!(binding.belongs_to_selection("trade", None));
        assert!(binding.belongs_to_selection(&scene, Some(&original)));
        assert!(!binding.belongs_to_selection(&scene, Some(&codex)));

        let binding_id = binding.id.clone();
        editor
            .edit(
                Command::ShareInScene {
                    scene: scene.clone(),
                    id: binding_id,
                },
                &app,
            )
            .unwrap();
        let mut tool = editor.vm.config_draft.bindings[0].clone();
        tool.id = "codex".into();
        tool.scene_ids.clear();
        tool.add_to_scene(&scene, Some(&codex));
        editor.vm.config_draft.bindings.push(tool);
        editor.save(&mut app, false).unwrap();

        let switched = app.switch_to_scene(
            &scene,
            Some(&codex),
            crate::scenes::switch::SwitchSource::PopupClick,
        );
        assert!(switched.switched);
        editor
            .edit(
                Command::RenameVariant {
                    scene: scene.clone(),
                    variant: codex.clone(),
                    name: "Codex CLI".into(),
                },
                &app,
            )
            .unwrap();
        editor.save(&mut app, false).unwrap();
        assert_eq!(
            app.config
                .scene(&scene)
                .unwrap()
                .preferred_variant()
                .unwrap()
                .name,
            "Codex CLI"
        );
        assert!(app.state_mgr.is_already_active(&scene, Some(&codex)));
        assert!(!editor.dirty());

        editor
            .edit(
                Command::DeleteVariant {
                    scene: scene.clone(),
                    variant: original,
                },
                &app,
            )
            .unwrap();
        editor.save(&mut app, false).unwrap();
        assert!(app.config.scene(&scene).unwrap().variants.is_empty());
        assert!(app
            .config
            .bindings
            .iter()
            .all(|binding| binding.belongs_to_selection(&scene, None)));
        assert!(app.config.bindings[0].belongs_to_selection("trade", None));
        assert!(app.state_mgr.active_scene_id().is_none());
        fs::remove_dir_all(folder).unwrap();
    }
}
