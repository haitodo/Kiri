use crate::config::WindowBinding;
use crate::diagnostics::{
    current_unix_ms, executable_name, is_verbose_title_logging, log_scene_switch,
};
use crate::platform::{FocusRequest, ShowCmd, WindowInstance, WindowRaise, WindowSystem};
use crate::scenes::bindings::{
    exe_paths_equal, matches_title_rule, matches_window_rule, BindingResolution,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT_SWITCH_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchSource {
    #[cfg(test)]
    Unspecified,
    BarClick,
    BarWheel,
    PopupClick,
    PopupWheel,
    Overflow,
    Hotkey,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PresentationPhase {
    Immediate,
    Verification,
}

impl PresentationPhase {
    pub(super) fn retries_focus(self) -> bool {
        matches!(self, Self::Verification)
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Verification => "verification",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SwitchTrace(Option<Rc<TraceState>>);

#[derive(Debug)]
struct TraceState {
    id: u64,
    started_at_ms: u64,
    started: Instant,
    events: RefCell<Vec<Value>>,
}

impl TraceState {
    fn flush(&self) {
        let events = self.events.take();
        if !events.is_empty() {
            log_scene_switch(&json!({
                "pid": std::process::id(),
                "switch_id": self.id,
                "started_at_ms": self.started_at_ms,
                "events": events,
            }));
        }
    }

    fn push(&self, event: &str, details: Value) {
        self.events.borrow_mut().push(json!({
            "elapsed_ms": self.started.elapsed().as_millis(),
            "event": event,
            "details": details,
        }));
    }
}

impl Drop for TraceState {
    fn drop(&mut self) {
        self.flush();
    }
}

impl SwitchTrace {
    pub(super) fn new() -> Self {
        if !is_verbose_title_logging() && !cfg!(debug_assertions) {
            return Self(None);
        }
        Self(Some(Rc::new(TraceState {
            id: NEXT_SWITCH_ID.fetch_add(1, Ordering::Relaxed),
            started_at_ms: current_unix_ms(),
            started: Instant::now(),
            events: RefCell::new(Vec::new()),
        })))
    }

    #[inline]
    pub(super) fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    #[inline]
    pub(super) fn record<F>(&self, event: &str, details: F)
    where
        F: FnOnce() -> Value,
    {
        if let Some(state) = &self.0 {
            state.push(event, details());
        }
    }

    pub(super) fn flush(&self) {
        if let Some(state) = &self.0 {
            state.flush();
        }
    }

    pub(super) fn show(
        &self,
        sys: &mut dyn WindowSystem,
        binding: &str,
        hwnd: u64,
        command: ShowCmd,
    ) -> Result<(), crate::platform::PlatformError> {
        let result = sys.show_window_async(hwnd, command);
        self.record("show_request", || {
            json!({
                "binding": binding,
                "hwnd": hwnd,
                "command": command,
                "response": result.as_ref().ok(),
                "error": result.as_ref().err().map(ToString::to_string),
            })
        });
        result.map(|_| ())
    }

    pub(super) fn raise(
        &self,
        sys: &mut dyn WindowSystem,
        phase: &str,
        hwnd: u64,
        method: WindowRaise,
    ) -> bool {
        let result = sys.bring_to_top(hwnd, method);
        self.record("raise_request", || {
            json!({
                "phase": phase, "hwnd": hwnd, "method": format!("{method:?}"), "response": result,
            })
        });
        result.accepted()
    }

    pub(super) fn focus(&self, sys: &mut dyn WindowSystem, phase: &str, hwnd: u64) -> FocusRequest {
        let Some(state) = &self.0 else {
            return sys.set_foreground_window(hwnd);
        };
        let before = sys.foreground_state();
        let response = sys.set_foreground_window(hwnd);
        state.push(
            "focus_request",
            json!({
                "phase": phase,
                "target": hwnd,
                "before": before,
                "response": response,
                "after": sys.foreground_state(),
            }),
        );
        response
    }

    pub(super) fn stacking(&self, phase: &str, targets: &[u64], sys: &dyn WindowSystem) {
        let Some(state) = &self.0 else {
            return;
        };
        const MAX_OVERLAPS_PER_WINDOW: usize = 12;
        let stack = if targets.is_empty() {
            Vec::new()
        } else {
            sys.window_stack()
        };
        let windows: Vec<_> = targets
            .iter()
            .map(|hwnd| {
                let Some(rank) = stack.iter().position(|entry| entry.hwnd == *hwnd) else {
                    return json!({"hwnd": hwnd, "in_visible_stack": false});
                };
                let target = &stack[rank];
                let mut overlaps = Vec::new();
                let mut overlap_count = 0;
                if !target.iconic {
                    for (above_rank, above) in stack[..rank].iter().enumerate() {
                        if above.iconic || above.rect.intersection_area(&target.rect) == 0 {
                            continue;
                        }
                        overlap_count += 1;
                        if overlaps.len() < MAX_OVERLAPS_PER_WINDOW {
                            overlaps.push(json!({
                                "rank": above_rank,
                                "window": above,
                                "in_scene": targets.contains(&above.hwnd)
                                    || above.owner.is_some_and(|owner| targets.contains(&owner)),
                            }));
                        }
                    }
                }
                json!({
                    "rank": rank,
                    "window": target,
                    "overlapping_above": overlaps,
                    "overlapping_above_count": overlap_count,
                })
            })
            .collect();
        state.push(
            "stacking",
            json!({
                "phase": phase,
                "foreground": sys.foreground_state(),
                "targets": windows,
            }),
        );
    }
}

pub(super) fn window_state(win: &WindowInstance) -> Value {
    json!({
        "hwnd": win.hwnd,
        "pid": win.pid,
        "exe": executable_name(&win.exe_path),
        "class": win.window_class,
        "monitor": win.monitor_id,
        "visible": win.is_visible,
        "iconic": win.is_iconic,
        "maximized": win.is_maximized,
        "topmost": win.is_topmost,
        "enabled": win.is_enabled,
        "hung": win.is_hung,
        "elevated": win.is_elevated,
        "z_order": win.z_order,
        "rect": win.window_rect,
    })
}

pub(super) fn binding_event(
    binding: &WindowBinding,
    in_scene: bool,
    resolution: &BindingResolution,
    windows: &[WindowInstance],
    claimed: &HashSet<u64>,
) -> Value {
    let rule = &binding.match_rule;
    let (result, candidates) = match resolution {
        BindingResolution::Resolved(win) => ("resolved", vec![window_state(win)]),
        BindingResolution::MovedToOtherMonitor(win) => ("other_monitor", vec![window_state(win)]),
        BindingResolution::AmbiguousSameMonitor(windows) => (
            "ambiguous",
            windows.iter().take(12).map(window_state).collect(),
        ),
        BindingResolution::AmbiguousCrossMonitor {
            target_candidate,
            other_candidates,
        } => (
            "ambiguous_cross_monitor",
            std::iter::once(target_candidate)
                .chain(other_candidates)
                .take(12)
                .map(window_state)
                .collect(),
        ),
        BindingResolution::NotFound => (
            "not_found",
            windows
                .iter()
                .filter(|win| {
                    !rule.exe_path.is_empty() && exe_paths_equal(&rule.exe_path, &win.exe_path)
                        || !rule.window_class.is_empty() && rule.window_class == win.window_class
                })
                .take(12)
                .map(|win| {
                    let mut state = window_state(win);
                    state["exe_matches"] = json!(
                        rule.exe_path.is_empty() || exe_paths_equal(&rule.exe_path, &win.exe_path)
                    );
                    state["class_matches"] = json!(
                        rule.window_class.is_empty() || rule.window_class == win.window_class
                    );
                    state["matches_rule"] = json!(matches_window_rule(rule, win));
                    state["title_matches"] =
                        json!(matches_title_rule(&rule.title_rule, &win.title));
                    state["claimed"] = json!(claimed.contains(&win.hwnd));
                    state
                })
                .collect(),
        ),
    };
    json!({
        "binding": binding.id,
        "in_scene": in_scene,
        "always_visible": binding.always_visible,
        "exe": executable_name(&rule.exe_path),
        "class": rule.window_class,
        "monitor_rule": rule.monitor_id,
        "title_rule": rule.title_rule.rule_type,
        "last_registered_hwnd": binding.instance_hint.as_ref().and_then(|hint| hint.last_hwnd),
        "result": result,
        "candidates": candidates,
    })
}
