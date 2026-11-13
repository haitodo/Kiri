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
use std::collections::{HashMap, HashSet};
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
    /// Process is running elevated (Administrator); UIPI prevents normal-privilege Kiri from controlling it.
    ElevatedProcess,
    /// Window is currently on a monitor other than `target_monitor`.
    OutsideTargetMonitor { window_monitor: String },
    /// Window title appears to contain digits (e.g. MT5 account number) or dynamic document/tab info.
    TitleMayContainSensitiveOrDynamicText,
    /// Multiple windows on the target monitor match this rule without disambiguation.
    AmbiguousSameMonitor { count: usize },
    /// Matching windows span multiple monitors without monitor/instance disambiguation.
    AmbiguousCrossMonitor { count: usize },
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

    /// Resolve a `WindowBinding` against currently enumerated windows and a single `target_monitor_id`.
    pub fn resolve_binding(
        &mut self,
        binding: &WindowBinding,
        all_windows: &[WindowInstance],
        target_monitor_id: &str,
    ) -> BindingResolution {
        let empty_claimed = HashSet::new();
        self.resolve_binding_single_monitor(binding, all_windows, target_monitor_id, &empty_claimed)
    }

    /// Resolve a `WindowBinding` on a single `target_monitor_id`, excluding any `claimed_hwnds`.
    pub fn resolve_binding_single_monitor(
        &mut self,
        binding: &WindowBinding,
        all_windows: &[WindowInstance],
        target_monitor_id: &str,
        claimed_hwnds: &HashSet<u64>,
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

        // Also check persisted `last_hwnd + last_pid + last_start_time` for live window before rule matching
        if let Some(ref hint) = binding.instance_hint {
            if let (Some(h_hwnd), Some(h_pid), Some(h_start)) =
                (hint.last_hwnd, hint.last_pid, hint.last_start_time)
            {
                if let Some(live) = all_windows.iter().find(|w| {
                    !claimed_hwnds.contains(&w.hwnd)
                        && w.matches_runtime_identity(h_hwnd, h_pid, h_start)
                        && (binding.match_rule.exe_path.is_empty()
                            || exe_paths_equal(&binding.match_rule.exe_path, &w.exe_path))
                }) {
                    self.bind_explicit(&binding.id, live);
                    if live.monitor_id == target_monitor_id {
                        return BindingResolution::Resolved(live.clone());
                    } else {
                        return BindingResolution::MovedToOtherMonitor(live.clone());
                    }
                }
            }
        }

        if let Some(ref req_mon) = binding.match_rule.monitor_id {
            if req_mon != target_monitor_id {
                return BindingResolution::NotFound;
            }
        }

        // Stage 2 (Section 4.2 #2): Match by rule across all candidate windows
        let matching: Vec<&WindowInstance> = all_windows
            .iter()
            .filter(|w| !claimed_hwnds.contains(&w.hwnd) && matches_window_rule(&binding.match_rule, w))
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

        // If `match_rule.monitor_id` explicitly pins this binding to `target_monitor_id` and there is 1 match on it
        if binding.match_rule.monitor_id.as_deref() == Some(target_monitor_id) && on_target.len() == 1 {
            let chosen = on_target.into_iter().next().unwrap();
            self.bind_explicit(&binding.id, &chosen);
            return BindingResolution::Resolved(chosen);
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

    /// Resolve a `WindowBinding` in multi-monitor mode (`MonitorPolicy::AllMonitors`).
    ///
    /// - While a registered window remains open, tracks its exact `(hwnd, pid, process_start_time)`.
    /// - When an app is closed and reopened, re-identifies the window using:
    ///   1. `exe_path`, `window_class`, `aumid`, and `title_rule` (e.g. tab/site keyword)
    ///   2. Target monitor placement (`match_rule.monitor_id` or `instance_hint.last_monitor_id`, e.g. left vs right monitor)
    ///   3. Exclusion of `claimed_hwnds` already resolved by more specific bindings in the same switch pass.
    pub fn resolve_binding_multi_monitor(
        &mut self,
        binding: &WindowBinding,
        all_windows: &[WindowInstance],
        connected_monitor_ids: &[String],
        claimed_hwnds: &HashSet<u64>,
    ) -> BindingResolution {
        if !binding.enabled {
            return BindingResolution::NotFound;
        }

        // Stage 1a: Re-validate existing cached runtime identity `(hwnd, pid, process_start_time)`
        if let Some(cached) = self.runtime_cache.get(&binding.id).cloned() {
            if let Some(live) = all_windows.iter().find(|w| {
                !claimed_hwnds.contains(&w.hwnd)
                    && w.matches_runtime_identity(cached.hwnd, cached.pid, cached.process_start_time)
            }) {
                return BindingResolution::Resolved(live.clone());
            } else {
                self.runtime_cache.remove(&binding.id);
            }
        }

        // Stage 1b: Check persisted `(last_hwnd, last_pid, last_start_time)` from registration if window is still alive
        if let Some(ref hint) = binding.instance_hint {
            if let (Some(h_hwnd), Some(h_pid), Some(h_start)) =
                (hint.last_hwnd, hint.last_pid, hint.last_start_time)
            {
                if let Some(live) = all_windows.iter().find(|w| {
                    !claimed_hwnds.contains(&w.hwnd)
                        && w.matches_runtime_identity(h_hwnd, h_pid, h_start)
                        && (binding.match_rule.exe_path.is_empty()
                            || exe_paths_equal(&binding.match_rule.exe_path, &w.exe_path))
                }) {
                    self.bind_explicit(&binding.id, live);
                    return BindingResolution::Resolved(live.clone());
                }
            }
        }

        // Stage 2: App was closed and reopened (or had no HWND hint) -> match by rule & monitor placement
        let matching: Vec<WindowInstance> = all_windows
            .iter()
            .filter(|w| !claimed_hwnds.contains(&w.hwnd) && matches_window_rule(&binding.match_rule, w))
            .cloned()
            .collect();

        if matching.is_empty() {
            return BindingResolution::NotFound;
        }

        // Determine expected monitor (explicit `match_rule.monitor_id` or fallback `instance_hint.last_monitor_id`)
        let explicit_mon = binding.match_rule.monitor_id.as_deref();
        let hint_mon = binding
            .instance_hint
            .as_ref()
            .and_then(|h| h.last_monitor_id.as_deref());

        if let Some(req_mon) = explicit_mon {
            let on_mon: Vec<WindowInstance> = matching
                .iter()
                .filter(|w| w.monitor_id == req_mon)
                .cloned()
                .collect();

            if on_mon.len() == 1 {
                let chosen = on_mon.into_iter().next().unwrap();
                self.bind_explicit(&binding.id, &chosen);
                return BindingResolution::Resolved(chosen);
            }
            if on_mon.len() > 1 {
                if let Some(ref hint) = binding.instance_hint {
                    if let (Some(h_pid), Some(h_start)) = (hint.last_pid, hint.last_start_time) {
                        let hinted: Vec<&WindowInstance> = on_mon
                            .iter()
                            .filter(|w| w.pid == h_pid && w.process_start_time == h_start)
                            .collect();
                        if hinted.len() == 1 {
                            let chosen = hinted[0].clone();
                            self.bind_explicit(&binding.id, &chosen);
                            return BindingResolution::Resolved(chosen);
                        }
                    }
                }
                return BindingResolution::AmbiguousSameMonitor(on_mon);
            }

            // `on_mon` is empty: if `req_mon` is currently connected, do not steal a window from another monitor.
            // Only fallback if `req_mon` itself is disconnected and there's a single unambiguous match.
            let mon_is_connected = connected_monitor_ids.iter().any(|m| m == req_mon);
            if mon_is_connected {
                return BindingResolution::NotFound;
            }
        } else if let Some(h_mon) = hint_mon {
            let on_mon: Vec<WindowInstance> = matching
                .iter()
                .filter(|w| w.monitor_id == h_mon)
                .cloned()
                .collect();
            if on_mon.len() == 1 {
                let chosen = on_mon.into_iter().next().unwrap();
                self.bind_explicit(&binding.id, &chosen);
                return BindingResolution::Resolved(chosen);
            }
            if on_mon.len() > 1 {
                return BindingResolution::AmbiguousSameMonitor(on_mon);
            }
        }

        // Monitor-agnostic or disconnected monitor fallback
        if matching.len() == 1 {
            let chosen = matching.into_iter().next().unwrap();
            self.bind_explicit(&binding.id, &chosen);
            return BindingResolution::Resolved(chosen);
        }

        BindingResolution::AmbiguousSameMonitor(matching)
    }
}

/// Extract a clean leading keyword (e.g. tab/site name or project name) from a window title
/// to help distinguish multiple windows of the same executable (such as Microsoft Edge) after restart.
pub fn extract_smart_title_keyword(title: &str) -> Option<String> {
    let cleaned = title.replace('\u{200b}', "");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Split on common browser/editor separators (` - ` or ` — `) and take the leading page/document segment
    let first_seg = trimmed
        .split(" - ")
        .next()
        .and_then(|s| s.split(" — ").next())
        .map(|s| s.trim())
        .unwrap_or(trimmed);

    if first_seg.is_empty() {
        return None;
    }

    // Avoid pure account numbers (4+ digits only)
    if first_seg.chars().all(|c| c.is_ascii_digit()) && first_seg.len() >= 4 {
        return None;
    }

    Some(first_seg.to_string())
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
        || lower_exe.ends_with("antigravity.exe")
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
