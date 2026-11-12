//! Window binding rule evaluation, runtime identity caching, ambiguity detection,
//! and registration safety warnings.
//!
//! Implements all 11 rules of Section 4.2 and Section 4.3 of spec.md:
//! 1. Re-validates existing `(hwnd, pid, process_start_time)` for live windows.
//! 2. Re-matches via normalized `exe_path`, `window_class`, `title_rule`, and `aumid` when HWND is gone.
//! 3. Never guesses when multiple windows match on the target monitor (`AmbiguousSameMonitor`).
//! 4. Never uses PID, HWND, or title alone as a permanent identifier.
//! 5. Evaluates only top-level unowned candidate windows.
//! 6. Attributes windows to monitors (using pre-minimized rect when iconic) and skips windows outside `target_monitor`.
//! 7. Cloaked / other virtual desktop / own windows are excluded by `WindowSystem`.
//! 8. Even if only 1 candidate is on `target_monitor`, if another monitor has a window matching the same rule
//!    and no explicit `instance_hint` disambiguates it, returns `AmbiguousCrossMonitor` for confirmation.
//! 9. Normalizes `exe_path` case-insensitively and checks `aumid` for packaged apps.
//! 10. Supports `any` / `exact` / `prefix` / `contains` / `regex` title rules.

use crate::config::{TitleRule, TitleRuleType, WindowBinding, WindowMatch};
use crate::platform::WindowInstance;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static REGEX_CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();

/// Runtime identity of a resolved window bound to a `WindowBinding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRuntimeIdentity {
    pub hwnd: u64,
    pub pid: u32,
    pub process_start_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingResolution {
    /// Unambiguously resolved to a window on `target_monitor`.
    Resolved(WindowInstance),
    /// Previously bound window still exists, but the user moved it to another monitor (Section 8.2).
    MovedToOtherMonitor(WindowInstance),
    /// Multiple windows on `target_monitor` matched the rule without a disambiguating hint (Section 4.2 #3, AT-10).
    AmbiguousSameMonitor(Vec<WindowInstance>),
    /// One window on `target_monitor` matched, but at least one window on another monitor also matched
    /// and the binding has not been explicitly pinned/confirmed to this instance (Section 4.2 #8).
    AmbiguousCrossMonitor {
        target_candidate: WindowInstance,
        other_candidates: Vec<WindowInstance>,
    },
    /// No matching window is currently open on `target_monitor`.
    NotFound,
}

/// Warnings presented to the user when registering a window in the Settings UI (Section 2.4 & 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationWarning {
    /// Window has no taskbar button (`!has_taskbar_button`), so user cannot manually restore it from taskbar.
    NoTaskbarButton,
    /// Process is running elevated (Administrator); UIPI prevents normal-privilege SceneDock from controlling it.
    ElevatedProcess,
    /// Window is currently on a monitor other than `target_monitor`.
    OutsideTargetMonitor { window_monitor: String },
    /// Window title appears to contain digits (e.g. MT5 account number) or dynamic document/tab info.
    TitleMayContainSensitiveOrDynamicText,
}

/// Normalize an executable path for case-insensitive comparison.
pub fn normalize_exe_path(path: &str) -> String {
    path.trim().replace('/', "\\").to_ascii_lowercase()
}

/// Zero-allocation case-insensitive executable path comparison normalizing `/` and `\`.
#[inline]
pub fn exe_paths_equal(a: &str, b: &str) -> bool {
    let a = a.trim().as_bytes();
    let b = b.trim().as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(&ca, &cb)| {
        let na = if ca == b'/' {
            b'\\'
        } else {
            ca.to_ascii_lowercase()
        };
        let nb = if cb == b'/' {
            b'\\'
        } else {
            cb.to_ascii_lowercase()
        };
        na == nb
    })
}

/// Evaluate whether a `TitleRule` matches a window title (with compiled regex caching).
pub fn matches_title_rule(rule: &TitleRule, title: &str) -> bool {
    match rule.rule_type {
        TitleRuleType::Any => true,
        TitleRuleType::Exact => rule
            .value
            .as_deref()
            .map(|v| title == v)
            .unwrap_or(true),
        TitleRuleType::Prefix => rule
            .value
            .as_deref()
            .map(|v| title.starts_with(v))
            .unwrap_or(true),
        TitleRuleType::Contains => rule
            .value
            .as_deref()
            .map(|v| title.contains(v))
            .unwrap_or(true),
        TitleRuleType::Regex => {
            let Some(pattern) = rule.value.as_deref() else {
                return true;
            };
            let cache_mutex = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(mut guard) = cache_mutex.lock() {
                let re_opt = guard
                    .entry(pattern.to_string())
                    .or_insert_with(|| Regex::new(pattern).ok());
                re_opt.as_ref().map(|re| re.is_match(title)).unwrap_or(false)
            } else {
                Regex::new(pattern)
                    .map(|re| re.is_match(title))
                    .unwrap_or(false)
            }
        }
    }
}

/// Evaluate whether a `WindowMatch` rule matches a `WindowInstance` (ignoring monitor placement).
pub fn matches_window_rule(rule: &WindowMatch, win: &WindowInstance) -> bool {
    // Packaged app AUMID check if specified in rule (Section 4.2 #9)
    if let Some(ref req_aumid) = rule.aumid {
        if !req_aumid.is_empty() {
            let Some(ref win_aumid) = win.aumid else {
                return false;
            };
            if !req_aumid.eq_ignore_ascii_case(win_aumid) {
                return false;
            }
        }
    }

    if !rule.exe_path.is_empty() && !exe_paths_equal(&rule.exe_path, &win.exe_path) {
        return false;
    }

    if !rule.window_class.is_empty() && rule.window_class != win.window_class {
        return false;
    }

    matches_title_rule(&rule.title_rule, &win.title)
}

/// Registry that caches runtime `(hwnd, pid, process_start_time)` identities for each `binding.id`.
#[derive(Debug, Default, Clone)]
pub struct BindingResolver {
    runtime_cache: HashMap<String, BoundRuntimeIdentity>,
}

impl BindingResolver {
    pub fn new() -> Self {
        Self {
            runtime_cache: HashMap::new(),
        }
    }

    /// Explicitly bind a `binding_id` to a specific live `WindowInstance` (e.g. when user picks from candidates).
    pub fn bind_explicit(&mut self, binding_id: &str, win: &WindowInstance) {
        self.runtime_cache.insert(
            binding_id.to_string(),
            BoundRuntimeIdentity {
                hwnd: win.hwnd,
                pid: win.pid,
                process_start_time: win.process_start_time,
            },
        );
    }

    pub fn clear_binding(&mut self, binding_id: &str) {
        self.runtime_cache.remove(binding_id);
    }

    pub fn cached_identity(&self, binding_id: &str) -> Option<&BoundRuntimeIdentity> {
        self.runtime_cache.get(binding_id)
    }

    /// Resolve a `WindowBinding` against currently enumerated windows and `target_monitor_id`.
    pub fn resolve_binding(
        &mut self,
        binding: &WindowBinding,
        all_windows: &[WindowInstance],
        target_monitor_id: &str,
    ) -> BindingResolution {
        if !binding.enabled {
            return BindingResolution::NotFound;
        }

        // Stage 1 (Section 4.2 #1): Re-validate existing cached runtime identity `(hwnd, pid, process_start_time)`
        if let Some(cached) = self.runtime_cache.get(&binding.id).cloned() {
            if let Some(live) = all_windows
                .iter()
                .find(|w| w.matches_runtime_identity(cached.hwnd, cached.pid, cached.process_start_time))
            {
                if live.monitor_id == target_monitor_id {
                    return BindingResolution::Resolved(live.clone());
                } else {
                    return BindingResolution::MovedToOtherMonitor(live.clone());
                }
            } else {
                // Previous HWND closed or process restarted; evict stale identity (AT-17)
                self.runtime_cache.remove(&binding.id);
            }
        }

        // Stage 2 (Section 4.2 #2): Match by rule across all candidate windows
        let matching: Vec<&WindowInstance> = all_windows
            .iter()
            .filter(|w| matches_window_rule(&binding.match_rule, w))
            .collect();

        if matching.is_empty() {
            return BindingResolution::NotFound;
        }

        let on_target: Vec<WindowInstance> = matching
            .iter()
            .filter(|w| w.monitor_id == target_monitor_id)
            .map(|&w| w.clone())
            .collect();

        let on_other: Vec<WindowInstance> = matching
            .iter()
            .filter(|w| w.monitor_id != target_monitor_id)
            .map(|&w| w.clone())
            .collect();

        if on_target.is_empty() {
            return BindingResolution::NotFound;
        }

        // Check if `instance_hint` disambiguates a specific window on target_monitor
        if let Some(ref hint) = binding.instance_hint {
            if let (Some(h_pid), Some(h_start)) = (hint.last_pid, hint.last_start_time) {
                let hinted: Vec<&WindowInstance> = on_target
                    .iter()
                    .filter(|w| w.pid == h_pid && w.process_start_time == h_start)
                    .collect();
                if hinted.len() == 1 {
                    let chosen = hinted[0].clone();
                    self.bind_explicit(&binding.id, &chosen);
                    return BindingResolution::Resolved(chosen);
                }
            }
            if let Some(ref disambig) = hint.disambiguator {
                if disambig == "confirmed_target_monitor" && on_target.len() == 1 {
                    let chosen = on_target[0].clone();
                    self.bind_explicit(&binding.id, &chosen);
                    return BindingResolution::Resolved(chosen);
                }
            }
        }

        // Multiple candidates on target monitor -> Never guess! (Section 4.2 #3, AT-10)
        if on_target.len() > 1 {
            return BindingResolution::AmbiguousSameMonitor(on_target);
        }

        // Exactly 1 candidate on target monitor, but another monitor also has a matching window (Section 4.2 #8)
        let target_candidate = on_target.into_iter().next().unwrap();
        if !on_other.is_empty() {
            return BindingResolution::AmbiguousCrossMonitor {
                target_candidate,
                other_candidates: on_other,
            };
        }

        self.bind_explicit(&binding.id, &target_candidate);
        BindingResolution::Resolved(target_candidate)
    }
}

/// Evaluate safety warnings when a user selects `win` for registration in the Settings UI (Section 2.4).
pub fn evaluate_registration_warnings(
    win: &WindowInstance,
    target_monitor_id: Option<&str>,
) -> Vec<RegistrationWarning> {
    let mut warnings = Vec::new();
    if !win.has_taskbar_button {
        warnings.push(RegistrationWarning::NoTaskbarButton);
    }
    if win.is_elevated {
        warnings.push(RegistrationWarning::ElevatedProcess);
    }
    if let Some(target_mon) = target_monitor_id {
        if win.monitor_id != target_mon {
            warnings.push(RegistrationWarning::OutsideTargetMonitor {
                window_monitor: win.monitor_id.clone(),
            });
        }
    }
    // Check if title contains account-like numbers (4+ consecutive digits) or dynamic separator (` - `)
    let has_digits = win
        .title
        .split(|c: char| !c.is_ascii_digit())
        .any(|seg| seg.len() >= 4);
    if has_digits || win.title.contains(" - ") {
        warnings.push(RegistrationWarning::TitleMayContainSensitiveOrDynamicText);
    }
    warnings
}

/// Suggest a resilient `TitleRule` for a window whose title changes frequently or contains account numbers
/// (Section 4.2 #10 & Section 6).
pub fn suggest_safe_title_rule(win: &WindowInstance) -> TitleRule {
    let lower_exe = win.exe_path.to_ascii_lowercase();
    if lower_exe.ends_with("terminal64.exe")
        || lower_exe.ends_with("ctrader.exe")
        || lower_exe.ends_with("code.exe")
        || lower_exe.ends_with("msedge.exe")
        || lower_exe.ends_with("chrome.exe")
    {
        return TitleRule::any();
    }
    if let Some((prefix, _)) = win.title.split_once(" - ") {
        if !prefix.trim().is_empty()
            && !prefix.chars().any(|c| c.is_ascii_digit())
        {
            return TitleRule::prefix(prefix.trim());
        }
    }
    TitleRule::any()
}
