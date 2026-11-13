//! Monitor enumeration, hardware-stable monitor identification via `QueryDisplayConfig`,
//! DPI scaling, launcher work-area clamping, edge snapping, and window-to-monitor attribution.
//!
//! Per Section 2.2 & 4.2 of spec.md:
//! - Never use raw `HMONITOR` or volatile `\\.\DISPLAY1` alone as the persistent monitor ID.
//! - Primary key: `DISPLAYCONFIG_TARGET_DEVICE_NAME::monitorDevicePath` obtained by matching
//!   `DISPLAYCONFIG_SOURCE_DEVICE_NAME::viewGdiDeviceName` with `MONITORINFOEXW::szDevice`.
//! - When multiple identical monitors yield duplicate target paths (or when `monitorDevicePath` is empty),
//!   append relative coordinates and resolution as a disambiguating secondary key.

use crate::config::{RectData, SavedPosition};
use crate::platform::MonitorInfoData;
use std::collections::HashMap;

/// Convert DIP (Device-Independent Pixels at 96 DPI) to physical pixels for `dpi`.
#[inline]
pub fn dip_to_px(dip: i32, dpi: u32) -> i32 {
    let d = dpi.max(96) as i64;
    ((dip as i64 * d + 48) / 96) as i32
}

/// Convert physical pixels at `dpi` back to DIP.
#[inline]
pub fn px_to_dip(px: i32, dpi: u32) -> i32 {
    let d = dpi.max(96) as i64;
    ((px as i64 * 96 + (d / 2)) / d) as i32
}

/// Clamp and optionally snap a launcher rectangle of size `(width_px, height_px)` into `work_area`.
/// If `snap_to_edges` is true, snaps to any edge within 12 DIP (`dip_to_px(12, dpi)`).
pub fn clamp_and_snap_in_work_area(
    mut x: i32,
    mut y: i32,
    width_px: i32,
    height_px: i32,
    work_area: RectData,
    dpi: u32,
    snap_to_edges: bool,
) -> (i32, i32) {
    let max_x = (work_area.right - width_px).max(work_area.left);
    let max_y = (work_area.bottom - height_px).max(work_area.top);

    x = x.clamp(work_area.left, max_x);
    y = y.clamp(work_area.top, max_y);

    if snap_to_edges {
        let snap_px = dip_to_px(12, dpi);
        if (x - work_area.left).abs() <= snap_px {
            x = work_area.left;
        } else if (max_x - x).abs() <= snap_px {
            x = max_x;
        }

        if (y - work_area.top).abs() <= snap_px {
            y = work_area.top;
        } else if (max_y - y).abs() <= snap_px {
            y = max_y;
        }
    }

    (x, y)
}

/// Default launcher position on a monitor (top-right area of work_area with a 24 DIP margin).
pub fn default_launcher_position_on_monitor(
    work_area: RectData,
    dpi: u32,
    width_px: i32,
    height_px: i32,
) -> (i32, i32) {
    let margin = dip_to_px(24, dpi);
    let x = (work_area.right - width_px - margin).max(work_area.left);
    let y = (work_area.top + margin).min((work_area.bottom - height_px).max(work_area.top));
    (x, y)
}

/// Resolve the launcher's screen position `(x, y, monitor_id, dpi)` from `SavedPosition` and connected monitors.
/// Falls back to the primary monitor if the saved monitor was disconnected (Section 2.2 & AT-13).
pub fn resolve_launcher_position(
    saved: Option<&SavedPosition>,
    monitors: &[MonitorInfoData],
    launcher_size_for_dpi: impl Fn(u32) -> (i32, i32),
    snap_to_edges: bool,
) -> (i32, i32, String, u32) {
    let primary = monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first());

    let Some(primary_mon) = primary else {
        let (w, h) = launcher_size_for_dpi(96);
        let _ = (w, h);
        return (100, 100, "FALLBACK-PRIMARY".to_string(), 96);
    };

    if let Some(pos) = saved {
        if let Some(mon) = monitors.iter().find(|m| m.monitor_id == pos.monitor_id) {
            let (w_px, h_px) = launcher_size_for_dpi(mon.dpi);
            let avail_w = (mon.work_area.width() - w_px).max(0) as f64;
            let avail_h = (mon.work_area.height() - h_px).max(0) as f64;
            let raw_x = mon.work_area.left + (pos.rel_x.clamp(0.0, 1.0) * avail_w).round() as i32;
            let raw_y = mon.work_area.top + (pos.rel_y.clamp(0.0, 1.0) * avail_h).round() as i32;
            let (x, y) = clamp_and_snap_in_work_area(
                raw_x,
                raw_y,
                w_px,
                h_px,
                mon.work_area,
                mon.dpi,
                snap_to_edges,
            );
            return (x, y, mon.monitor_id.clone(), mon.dpi);
        }
    }

    // Fallback to primary monitor when saved is None or saved monitor is disconnected
    let (w_px, h_px) = launcher_size_for_dpi(primary_mon.dpi);
    let (x, y) =
        default_launcher_position_on_monitor(primary_mon.work_area, primary_mon.dpi, w_px, h_px);
    (
        x,
        y,
        primary_mon.monitor_id.clone(),
        primary_mon.dpi,
    )
}

/// Compute `SavedPosition` from the launcher's current screen top-left `(x, y)` and size `(width_px, height_px)`.
pub fn compute_saved_position(
    x: i32,
    y: i32,
    width_px: i32,
    height_px: i32,
    monitors: &[MonitorInfoData],
    snap_to_edges: bool,
) -> Option<SavedPosition> {
    let (_, _, _, _, saved) = compute_saved_position_with_dpi(
        x,
        y,
        width_px,
        height_px,
        None,
        monitors,
        |_dpi| (width_px, height_px),
        snap_to_edges,
    )?;
    Some(saved)
}

/// Compute the target monitor, DPI-scaled `(width_px, height_px)`, clamped `(x, y)`, and `SavedPosition`
/// when a launcher drag finishes. If `cursor_anchor` is provided, attributes the drop to the monitor
/// containing the cursor (so dragging near a monitor edge does not accidentally jump to an adjacent monitor).
pub fn compute_saved_position_with_dpi(
    x: i32,
    y: i32,
    current_width_px: i32,
    current_height_px: i32,
    cursor_anchor: Option<(i32, i32)>,
    monitors: &[MonitorInfoData],
    launcher_size_for_dpi: impl Fn(u32) -> (i32, i32),
    snap_to_edges: bool,
) -> Option<(i32, i32, i32, i32, SavedPosition)> {
    let mon = if let Some((cx, cy)) = cursor_anchor {
        find_monitor_for_point(cx, cy, monitors)?
    } else {
        let launcher_rect = RectData {
            left: x,
            top: y,
            right: x + current_width_px,
            bottom: y + current_height_px,
        };
        find_best_monitor_for_rect(&launcher_rect, monitors)?
    };

    let (w_px, h_px) = launcher_size_for_dpi(mon.dpi);
    let (clamped_x, clamped_y) = clamp_and_snap_in_work_area(
        x,
        y,
        w_px,
        h_px,
        mon.work_area,
        mon.dpi,
        snap_to_edges,
    );

    let avail_w = (mon.work_area.width() - w_px).max(1) as f64;
    let avail_h = (mon.work_area.height() - h_px).max(1) as f64;
    let rel_x = ((clamped_x - mon.work_area.left) as f64 / avail_w).clamp(0.0, 1.0);
    let rel_y = ((clamped_y - mon.work_area.top) as f64 / avail_h).clamp(0.0, 1.0);

    let saved = SavedPosition {
        monitor_id: mon.monitor_id.clone(),
        rel_x,
        rel_y,
        dpi: mon.dpi,
    };

    Some((clamped_x, clamped_y, w_px, h_px, saved))
}

/// Find the monitor containing `(px, py)`, falling back to the monitor whose `monitor_rect` is closest to `(px, py)`.
pub fn find_monitor_for_point<'a>(
    px: i32,
    py: i32,
    monitors: &'a [MonitorInfoData],
) -> Option<&'a MonitorInfoData> {
    if monitors.is_empty() {
        return None;
    }

    if let Some(mon) = monitors.iter().find(|m| {
        px >= m.monitor_rect.left
            && px < m.monitor_rect.right
            && py >= m.monitor_rect.top
            && py < m.monitor_rect.bottom
    }) {
        return Some(mon);
    }

    monitors.iter().min_by_key(|mon| {
        let clamped_x = px.clamp(mon.monitor_rect.left, mon.monitor_rect.right);
        let clamped_y = py.clamp(mon.monitor_rect.top, mon.monitor_rect.bottom);
        let dx = (px - clamped_x) as i64;
        let dy = (py - clamped_y) as i64;
        dx * dx + dy * dy
    })
}

/// Attribute a rectangle (e.g. a non-iconic window rect or pre-minimized normal rect) to the monitor
/// with the largest intersection area, falling back to nearest monitor edge / center distance.
pub fn find_best_monitor_for_rect<'a>(
    rect: &RectData,
    monitors: &'a [MonitorInfoData],
) -> Option<&'a MonitorInfoData> {
    if monitors.is_empty() {
        return None;
    }

    let mut best_mon = None;
    let mut best_area = 0i64;

    for mon in monitors {
        let area = rect.intersection_area(&mon.monitor_rect);
        if area > best_area {
            best_area = area;
            best_mon = Some(mon);
        }
    }

    if best_mon.is_some() {
        return best_mon;
    }

    // Fallback: closest monitor rect to rect center, breaking ties by center distance
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;

    monitors.iter().min_by_key(|mon| {
        let clamped_x = cx.clamp(mon.monitor_rect.left, mon.monitor_rect.right);
        let clamped_y = cy.clamp(mon.monitor_rect.top, mon.monitor_rect.bottom);
        let edge_dx = (cx - clamped_x) as i64;
        let edge_dy = (cy - clamped_y) as i64;
        let edge_dist_sq = edge_dx * edge_dx + edge_dy * edge_dy;

        let mcx = (mon.monitor_rect.left as i64 + mon.monitor_rect.right as i64) / 2;
        let mcy = (mon.monitor_rect.top as i64 + mon.monitor_rect.bottom as i64) / 2;
        let center_dx = cx as i64 - mcx;
        let center_dy = cy as i64 - mcy;
        let center_dist_sq = center_dx * center_dx + center_dy * center_dy;

        (edge_dist_sq, center_dist_sq)
    })
}

#[cfg(windows)]
pub fn enumerate_connected_monitors() -> Vec<MonitorInfoData> {
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    #[derive(Clone, PartialEq, Eq)]
    struct RawMon {
        hmon: isize,
        gdi_name: String,
        monitor_rect: RectData,
        work_area: RectData,
        dpi: u32,
        is_primary: bool,
    }

    struct MonitorCache {
        raw: Vec<RawMon>,
        monitors: Vec<MonitorInfoData>,
    }

    static MONITOR_CACHE: OnceLock<Mutex<Option<MonitorCache>>> = OnceLock::new();

    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<RawMon>);
        let mut mi = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            szDevice: [0u16; 32],
        };

        if GetMonitorInfoW(hmon, &mut mi.monitorInfo as *mut MONITORINFO).as_bool() {
            let gdi_name = wide_null_to_string(&mi.szDevice);
            let mut dpi_x = 96u32;
            let mut dpi_y = 96u32;
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);

            let rc_mon = mi.monitorInfo.rcMonitor;
            let rc_work = mi.monitorInfo.rcWork;
            let is_primary = (mi.monitorInfo.dwFlags & 1) != 0; // MONITORINFOF_PRIMARY

            list.push(RawMon {
                hmon: hmon.0 as isize,
                gdi_name,
                monitor_rect: RectData {
                    left: rc_mon.left,
                    top: rc_mon.top,
                    right: rc_mon.right,
                    bottom: rc_mon.bottom,
                },
                work_area: RectData {
                    left: rc_work.left,
                    top: rc_work.top,
                    right: rc_work.right,
                    bottom: rc_work.bottom,
                },
                dpi: dpi_x.max(96),
                is_primary,
            });
        }
        BOOL(1)
    }

    let mut raw_list: Vec<RawMon> = Vec::with_capacity(4);
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_proc),
            LPARAM(&mut raw_list as *mut Vec<RawMon> as isize),
        );
    }

    let cache_mutex = MONITOR_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache_mutex.lock() {
        if let Some(ref cached) = *guard {
            if cached.raw == raw_list {
                return cached.monitors.clone();
            }
        }
    }

    let target_map = query_display_config_map();

    // Count occurrences of each base hardware device path to detect identical dual-monitor models
    let mut base_id_counts: HashMap<String, usize> = HashMap::new();
    for raw in &raw_list {
        let base_id = target_map
            .get(&raw.gdi_name)
            .map(|(path, _)| path.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "POS:{},{}:{}x{}",
                    raw.monitor_rect.left,
                    raw.monitor_rect.top,
                    raw.monitor_rect.width(),
                    raw.monitor_rect.height()
                )
            });
        *base_id_counts.entry(base_id).or_insert(0) += 1;
    }

    let monitors: Vec<MonitorInfoData> = raw_list
        .iter()
        .map(|raw| {
            let (hw_path, friendly) = target_map
                .get(&raw.gdi_name)
                .cloned()
                .unwrap_or_else(|| (String::new(), raw.gdi_name.clone()));

            let base_id = if hw_path.is_empty() {
                format!(
                    "POS:{},{}:{}x{}",
                    raw.monitor_rect.left,
                    raw.monitor_rect.top,
                    raw.monitor_rect.width(),
                    raw.monitor_rect.height()
                )
            } else {
                hw_path
            };

            let monitor_id = if base_id_counts.get(&base_id).copied().unwrap_or(0) > 1 {
                // Disambiguate identical monitor models using relative placement & resolution (Section 2.2)
                format!(
                    "{}@({},{}:{}x{})",
                    base_id,
                    raw.monitor_rect.left,
                    raw.monitor_rect.top,
                    raw.monitor_rect.width(),
                    raw.monitor_rect.height()
                )
            } else {
                base_id
            };

            let friendly_name = if friendly.is_empty() {
                raw.gdi_name.clone()
            } else {
                friendly
            };

            MonitorInfoData {
                monitor_id,
                friendly_name,
                monitor_rect: raw.monitor_rect,
                work_area: raw.work_area,
                dpi: raw.dpi,
                is_primary: raw.is_primary,
            }
        })
        .collect();

    if let Ok(mut guard) = cache_mutex.lock() {
        *guard = Some(MonitorCache {
            raw: raw_list,
            monitors: monitors.clone(),
        });
    }

    monitors
}

#[cfg(not(windows))]
pub fn enumerate_connected_monitors() -> Vec<MonitorInfoData> {
    Vec::new()
}

/// Maps GDI device name (e.g. `\\.\DISPLAY1`) -> `(monitorDevicePath, monitorFriendlyDeviceName)`
/// using `QueryDisplayConfig` and `DisplayConfigGetDeviceInfo`.
#[cfg(windows)]
fn query_display_config_map() -> HashMap<String, (String, String)> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::WIN32_ERROR;

    let mut map = HashMap::new();
    unsafe {
        let mut num_paths = 0u32;
        let mut num_modes = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes)
            != WIN32_ERROR(0)
        {
            return map;
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];

        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            None,
        ) != WIN32_ERROR(0)
        {
            return map;
        }

        paths.truncate(num_paths as usize);

        for path in paths {
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                viewGdiDeviceName: [0u16; 32],
            };

            let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };

            let src_ok = DisplayConfigGetDeviceInfo(&mut source_name.header) == 0;
            let tgt_ok = DisplayConfigGetDeviceInfo(&mut target_name.header) == 0;

            if src_ok && tgt_ok {
                let gdi = wide_null_to_string(&source_name.viewGdiDeviceName);
                let dev_path = wide_null_to_string(&target_name.monitorDevicePath);
                let friendly = wide_null_to_string(&target_name.monitorFriendlyDeviceName);
                if !gdi.is_empty() {
                    map.insert(gdi, (dev_path, friendly));
                }
            }
        }
    }
    map
}

#[cfg(windows)]
pub(crate) fn wide_null_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakeWindowSystem;
    use crate::platform::WindowSystem;

    #[test]
    fn test_dip_px_conversion_and_edge_snap() {
        assert_eq!(dip_to_px(32, 96), 32);
        assert_eq!(dip_to_px(32, 144), 48); // 150% DPI
        assert_eq!(px_to_dip(48, 144), 32);

        let work = RectData {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        // Within 12 DIP of left edge -> snaps to 0 when snap_to_edges=true
        let (x, y) = clamp_and_snap_in_work_area(8, 25, 126, 38, work, 96, true);
        assert_eq!(x, 0);
        assert_eq!(y, 25);

        // When snap_to_edges=false -> stays at 8
        let (x2, _) = clamp_and_snap_in_work_area(8, 25, 126, 38, work, 96, false);
        assert_eq!(x2, 8);
    }

    #[test]
    fn test_monitor_disconnect_falls_back_to_primary() {
        let sys = FakeWindowSystem::new_dual_monitor();
        let monitors = sys.monitors();
        let saved = SavedPosition {
            monitor_id: "DISCONNECTED-MON-3".to_string(),
            rel_x: 0.5,
            rel_y: 0.5,
            dpi: 96,
        };
        let (x, y, mon_id, _) =
            resolve_launcher_position(Some(&saved), &monitors, |_dpi| (126, 38), false);
        assert_eq!(mon_id, "MON-1-PRIMARY");
        assert!(x >= 0 && x + 126 <= 1920);
        assert!(y >= 0 && y + 38 <= 1040);
    }

    #[test]
    fn test_mixed_dpi_4k_150_and_2k_100_drag_and_edge_attribution() {
        let monitors = vec![
            MonitorInfoData {
                monitor_id: "MON-4K-150".to_string(),
                friendly_name: "4K Monitor".to_string(),
                monitor_rect: RectData {
                    left: 0,
                    top: 0,
                    right: 3840,
                    bottom: 2160,
                },
                work_area: RectData {
                    left: 0,
                    top: 0,
                    right: 3840,
                    bottom: 2088,
                },
                dpi: 144, // 150%
                is_primary: true,
            },
            MonitorInfoData {
                monitor_id: "MON-2K-100".to_string(),
                friendly_name: "2K Monitor".to_string(),
                monitor_rect: RectData {
                    left: 3840,
                    top: 0,
                    right: 6400,
                    bottom: 1440,
                },
                work_area: RectData {
                    left: 3840,
                    top: 0,
                    right: 6400,
                    bottom: 1392,
                },
                dpi: 96, // 100%
                is_primary: false,
            },
        ];

        let size_for_dpi = |dpi: u32| (dip_to_px(126, dpi), dip_to_px(38, dpi));

        // 1. Drag near the right edge of the 4K monitor (cursor at x=3800 on 4K monitor):
        // Even if raw window rect (3788..3977) overhangs into 2K monitor, cursor_anchor keeps it on 4K monitor.
        let (cx, cy, w, h, saved_4k) = compute_saved_position_with_dpi(
            3788,
            100,
            189,
            57,
            Some((3800, 120)),
            &monitors,
            size_for_dpi,
            false,
        )
        .unwrap();
        assert_eq!(saved_4k.monitor_id, "MON-4K-150");
        assert_eq!(saved_4k.dpi, 144);
        assert_eq!((w, h), (189, 57));
        assert_eq!(cx, 3840 - 189);
        assert_eq!(cy, 100);

        // 2. Drag onto the 2K monitor (cursor at x=4200 on 2K monitor):
        // Resolves using 2K monitor's 96 DPI dimensions (126x38) with zero round-trip position shift.
        let (cx2, cy2, w2, h2, saved_2k) = compute_saved_position_with_dpi(
            4190,
            200,
            189,
            57,
            Some((4200, 215)),
            &monitors,
            size_for_dpi,
            false,
        )
        .unwrap();
        assert_eq!(saved_2k.monitor_id, "MON-2K-100");
        assert_eq!(saved_2k.dpi, 96);
        assert_eq!((w2, h2), (126, 38));
        assert_eq!((cx2, cy2), (4190, 200));

        let (rx, ry, rmon, rdpi) =
            resolve_launcher_position(Some(&saved_2k), &monitors, size_for_dpi, false);
        assert_eq!((rx, ry), (cx2, cy2));
        assert_eq!(rmon, "MON-2K-100");
        assert_eq!(rdpi, 96);
    }
}
