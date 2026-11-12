//! Manual window layout snapshot saving and restoration (`LayoutSnapshot`, Section 5.2 & FR-12).
//!
//! Per Section 5.2 & 6 of spec.md:
//! - Normal scene switches NEVER call `SetWindowPlacement` (they preserve the window's current placement).
//! - Explicit "Save Layout" records each bound window's `normal_rect` as relative ratios `(0.0..=1.0)`
//!   within the target monitor's work area (`rcWork`), along with `maximized` state and monitor DPI.
//! - Explicit "Restore Layout" applies saved placements only to windows that have saved snapshot entries.
//! - PID and HWND are never used as persistent keys (`binding_id` is used).

use crate::config::{AppConfig, LayoutSnapshot, RectData, SavedWindowLayoutEntry};
use crate::platform::WindowSystem;
use crate::scenes::bindings::{BindingResolution, BindingResolver};

/// Capture the current layout of all resolved windows belonging to `scene_id` on `target_monitor_id`.
pub fn capture_scene_layout(
    scene_id: &str,
    target_monitor_id: &str,
    config: &AppConfig,
    resolver: &mut BindingResolver,
    sys: &dyn WindowSystem,
) -> Option<LayoutSnapshot> {
    let monitors = sys.monitors();
    let mon = monitors.iter().find(|m| m.monitor_id == target_monitor_id)?;
    let work_w = mon.work_area.width().max(1) as f64;
    let work_h = mon.work_area.height().max(1) as f64;

    let all_windows = sys.enumerate_windows();
    let mut entries = Vec::new();

    for binding in &config.bindings {
        if !binding.enabled || !binding.scene_ids.iter().any(|id| id == scene_id) {
            continue;
        }
        if let BindingResolution::Resolved(win) =
            resolver.resolve_binding(binding, &all_windows, target_monitor_id)
        {
            // WINDOWPLACEMENT.rcNormalPosition is already in workspace coordinates (relative to work area origin)
            let rel_left = (win.normal_rect.left - mon.work_area.left) as f64 / work_w;
            let rel_top = (win.normal_rect.top - mon.work_area.top) as f64 / work_h;
            let rel_right = (win.normal_rect.right - mon.work_area.left) as f64 / work_w;
            let rel_bottom = (win.normal_rect.bottom - mon.work_area.top) as f64 / work_h;

            entries.push(SavedWindowLayoutEntry {
                binding_id: binding.id.clone(),
                rel_left,
                rel_top,
                rel_right,
                rel_bottom,
                maximized: win.is_maximized,
            });
        }
    }

    Some(LayoutSnapshot {
        scene_id: scene_id.to_string(),
        monitor_id: target_monitor_id.to_string(),
        dpi: mon.dpi,
        entries,
    })
}

/// Upsert a `LayoutSnapshot` into `config.layouts`.
pub fn save_layout_into_config(config: &mut AppConfig, snapshot: LayoutSnapshot) {
    if let Some(existing) = config
        .layouts
        .iter_mut()
        .find(|l| l.scene_id == snapshot.scene_id && l.monitor_id == snapshot.monitor_id)
    {
        *existing = snapshot;
    } else {
        config.layouts.push(snapshot);
    }
}

/// Restore the saved `LayoutSnapshot` for `(scene_id, target_monitor_id)` onto currently resolved windows.
/// Returns the number of windows whose placement was updated.
pub fn apply_saved_scene_layout(
    scene_id: &str,
    target_monitor_id: &str,
    config: &AppConfig,
    resolver: &mut BindingResolver,
    sys: &mut dyn WindowSystem,
) -> usize {
    let Some(snapshot) = config
        .layouts
        .iter()
        .find(|l| l.scene_id == scene_id && l.monitor_id == target_monitor_id)
    else {
        return 0;
    };

    let monitors = sys.monitors();
    let Some(mon) = monitors.iter().find(|m| m.monitor_id == target_monitor_id) else {
        return 0;
    };

    let work_w = mon.work_area.width().max(1) as f64;
    let work_h = mon.work_area.height().max(1) as f64;
    let all_windows = sys.enumerate_windows();

    let mut applied = 0;
    for entry in &snapshot.entries {
        let Some(binding) = config
            .bindings
            .iter()
            .find(|b| b.id == entry.binding_id && b.enabled)
        else {
            continue;
        };

        if let BindingResolution::Resolved(win) =
            resolver.resolve_binding(binding, &all_windows, target_monitor_id)
        {
            let target_rect = RectData {
                left: mon.work_area.left + (entry.rel_left * work_w).round() as i32,
                top: mon.work_area.top + (entry.rel_top * work_h).round() as i32,
                right: mon.work_area.left + (entry.rel_right * work_w).round() as i32,
                bottom: mon.work_area.top + (entry.rel_bottom * work_h).round() as i32,
            };

            if sys
                .set_window_placement(win.hwnd, target_rect, entry.maximized)
                .is_ok()
            {
                applied += 1;
            }
        }
    }

    applied
}
