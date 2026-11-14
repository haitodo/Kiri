use super::{is_autostart_enabled, RegistrationWarning, SettingsViewModel};
use crate::app::AppController;
use crate::config::{
    AppConfig, CursorPopupPosition, LauncherConfig, MonitorPolicy, Orientation, Scene, Shortcut,
    TitleRule, TitleRuleType, WindowBinding,
};
use crate::diagnostics::executable_name;
use crate::launcher::paint::{is_builtin_icon, BUILTIN_ICONS};
use crate::platform::WindowInstance;
use crate::scenes::switch::{format_switch_warnings, preview_scene_switch};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
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
    AddWindows {
        scene: String,
        candidates: Vec<String>,
    },
    RemoveFromScene {
        scene: String,
        id: String,
    },
    EditBinding {
        id: String,
        scenes: Vec<String>,
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
                        config.bindings.iter().any(|b| b.id == entry.binding_id)
                            && !self
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
                            && !config.bindings.iter().any(|b| b.scene_ids.contains(&id))
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
                    let was_member = binding.scene_ids.contains(&id);
                    binding.scene_ids.retain(|s| s != &id);
                    !was_member || binding.always_visible || !binding.scene_ids.is_empty()
                });
                self.vm.refresh_annotations();
            }
            DeleteLayout { id } => {
                require_scene(&self.vm.config_draft, &id)?;
                self.vm.delete_scene_layouts(&id);
                self.deleted_layouts.insert(id);
            }
            AddWindows { scene, candidates } => {
                require_scene(&self.vm.config_draft, &scene)?;
                let windows = self.live_candidates(&candidates, app)?;
                if windows.iter().any(|win| self.ambiguous_candidate(win.hwnd)) {
                    return Err("一致する登録済みウィンドウがあります。「登録済みウィンドウを管理」から対象を確認してください。".into());
                }
                for win in windows {
                    self.vm.add_window_to_scene(&win, &scene);
                }
            }
            RemoveFromScene { scene, id } => {
                require_scene(&self.vm.config_draft, &scene)?;
                self.vm.unbind_window_from_scene(&id, &scene);
                self.detached.insert((scene, id));
            }
            EditBinding {
                id,
                scenes,
                always,
                enabled,
                rule,
                monitor,
            } => {
                self.edit_binding(&id, scenes, always, enabled, rule, monitor)?;
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
                if policy == MonitorPolicy::Fixed
                    && !monitor
                        .as_ref()
                        .is_some_and(|id| self.vm.monitors.iter().any(|mon| &mon.monitor_id == id))
                {
                    return Err("接続されている画面を選んでください。".into());
                }
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
            if let Some(reason) = self.candidate_block_reason(&candidate.instance) {
                return Err(reason.into());
            }
            Ok(candidate.instance.clone())
        }).collect()
    }

    fn candidate_block_reason(&self, win: &WindowInstance) -> Option<&'static str> {
        if win.exe_path.is_empty() && win.aumid.is_none() {
            Some("アプリを識別できません")
        } else if win.is_elevated {
            Some("管理者権限で実行中")
        } else if self.vm.config_draft.target.monitor_policy != MonitorPolicy::AllMonitors
            && self.vm.target_monitor().as_deref() != Some(win.monitor_id.as_str())
        {
            Some("操作する画面の外")
        } else {
            None
        }
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
        always: bool,
        enabled: bool,
        mut rule: TitleRule,
        monitor: Option<String>,
    ) -> Result<(), String> {
        let config = &mut self.vm.config_draft;
        for id in &scenes {
            require_scene(config, id)?;
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
        if scenes.is_empty() && enabled && !always {
            return Err("シーンを選ぶか、この登録を無効にしてください。".into());
        }
        if rule != binding.match_rule.title_rule || monitor != binding.match_rule.monitor_id {
            binding.instance_hint = None;
        }
        for scene in &binding.scene_ids {
            if !scenes.contains(scene) {
                self.detached.insert((scene.clone(), id.into()));
            }
        }
        binding.match_rule.title_rule = rule;
        binding.match_rule.monitor_id = monitor;
        binding.always_visible = always;
        binding.enabled = enabled;
        binding.scene_ids = scenes;
        for layout in config
            .layouts
            .iter_mut()
            .filter(|l| !binding.scene_ids.contains(&l.scene_id))
        {
            layout.entries.retain(|entry| entry.binding_id != id);
        }
        self.vm.refresh_annotations();
        Ok(())
    }

    pub(super) fn inspect(&self, scene: &str, app: &AppController) -> Value {
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
                    "blocked": self.candidate_block_reason(&row.instance),
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
                "scenes": binding.scene_ids, "always": binding.always_visible, "enabled": binding.enabled,
                "running": row.is_running, "warnings": row.warnings.iter().filter_map(warning_label).collect::<Vec<_>>(),
                "ambiguous": row.ambiguous_candidates.iter().map(candidate_key).collect::<Vec<_>>(),
                "rule": binding.match_rule.title_rule, "monitor": binding.match_rule.monitor_id,
            })
        }).collect();
        let scenes: Vec<_> = config.ordered_scenes().iter().map(|scene| {
            let (layouts, _) = self.vm.scene_layout_summary(&scene.id);
            json!({ "id": scene.id, "name": scene.name, "icon": scene.icon,
                "pinned": scene.pinned, "layouts": layouts,
                "hotkey": self.vm.get_scene_hotkey(&scene.id).map(|(mods, key)| json!({"mods": mods, "key": key})),
            })
        }).collect();
        json!({
            "scenes": scenes, "bindings": bindings, "candidates": candidates,
            "monitors": monitors, "target": config.target, "effective_monitor": self.vm.target_monitor(),
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

fn require_scene(config: &AppConfig, id: &str) -> Result<(), String> {
    config
        .scenes
        .iter()
        .any(|s| s.id == id)
        .then_some(())
        .ok_or_else(|| "シーンが見つかりません。".into())
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
        RegistrationWarning::OutsideTargetMonitor { .. } => "操作する画面の外".into(),
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
}
