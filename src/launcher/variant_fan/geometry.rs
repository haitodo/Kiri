//! Pure-Rust geometry and polar hit-testing for the interactive variant fan.

use crate::config::RectData;
use crate::launcher::input::point_in_rect;
use crate::platform::monitors::dip_to_px;
use std::f64::consts::{FRAC_PI_2, PI};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanAxis {
    /// Horizontal bar / grid tile: expand upward or downward.
    Vertical,
    /// Vertical bar / list row: expand leftward or rightward.
    Horizontal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FanItemLayout {
    pub index: usize,
    pub rect: RectData,
    pub center_x: i32,
    pub center_y: i32,
    /// Angle relative to `base_angle` in `[-PI, PI]`.
    pub rel_angle: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FanLayout {
    pub origin_x: i32,
    pub origin_y: i32,
    pub deadzone_radius: i32,
    pub base_angle: f64,
    pub is_list_fallback: bool,
    pub bounds: RectData,
    pub items: Vec<FanItemLayout>,
}

const PILL_HEIGHT_DIP: i32 = 28;
const PILL_MIN_WIDTH_DIP: i32 = 60;
const PILL_MAX_WIDTH_DIP: i32 = 190;
const PILL_HPAD_DIP: i32 = 24;
const PILL_GAP_DIP: i32 = 6;
const DEADZONE_RADIUS_DIP: i32 = 18;
const FAN_RADIUS_DIP: i32 = 76;
const EDGE_MARGIN_DIP: i32 = 6;
const WINDOW_SHADOW_PAD_DIP: i32 = 18;
const MAX_FAN_ITEMS: usize = 5;

impl FanLayout {
    pub fn compute(
        anchor: RectData,
        label_widths_px: &[i32],
        work_area: RectData,
        axis: FanAxis,
        dpi: u32,
    ) -> Self {
        let dpi = dpi.max(96);
        let origin_x = (anchor.left + anchor.right) / 2;
        let origin_y = (anchor.top + anchor.bottom) / 2;
        let deadzone_radius = dip_to_px(DEADZONE_RADIUS_DIP, dpi);

        let base_angle = match axis {
            FanAxis::Vertical => {
                let space_above = origin_y - work_area.top;
                let space_below = work_area.bottom - origin_y;
                if space_above >= space_below {
                    -FRAC_PI_2
                } else {
                    FRAC_PI_2
                }
            }
            FanAxis::Horizontal => {
                let space_left = origin_x - work_area.left;
                let space_right = work_area.right - origin_x;
                if space_right >= space_left {
                    0.0
                } else {
                    PI
                }
            }
        };

        let count = label_widths_px.len();
        if count == 0 {
            let pad = dip_to_px(WINDOW_SHADOW_PAD_DIP, dpi) + deadzone_radius;
            return Self {
                origin_x,
                origin_y,
                deadzone_radius,
                base_angle,
                is_list_fallback: false,
                bounds: RectData {
                    left: origin_x - pad,
                    top: origin_y - pad,
                    right: origin_x + pad,
                    bottom: origin_y + pad,
                },
                items: Vec::new(),
            };
        }

        let pill_h = dip_to_px(PILL_HEIGHT_DIP, dpi).max(20);
        let min_w = dip_to_px(PILL_MIN_WIDTH_DIP, dpi);
        let max_w = dip_to_px(PILL_MAX_WIDTH_DIP, dpi)
            .min((work_area.width() - dip_to_px(EDGE_MARGIN_DIP * 2, dpi)).max(min_w));
        let hpad = dip_to_px(PILL_HPAD_DIP, dpi);
        let gap = dip_to_px(PILL_GAP_DIP, dpi);
        let edge_margin = dip_to_px(EDGE_MARGIN_DIP, dpi);

        let pill_widths: Vec<i32> = label_widths_px
            .iter()
            .map(|&w| (w + hpad).clamp(min_w, max_w))
            .collect();

        let is_list_fallback = count > MAX_FAN_ITEMS;
        let mut rects = if is_list_fallback {
            layout_list(
                origin_x,
                origin_y,
                deadzone_radius,
                base_angle,
                axis,
                &pill_widths,
                pill_h,
                gap,
                work_area,
                edge_margin,
            )
        } else {
            layout_arc(
                origin_x,
                origin_y,
                deadzone_radius,
                base_angle,
                axis,
                &pill_widths,
                pill_h,
                gap,
                dpi,
            )
        };

        clamp_rects_to_work_area(&mut rects, work_area, edge_margin);

        let items: Vec<FanItemLayout> = rects
            .into_iter()
            .enumerate()
            .map(|(index, rect)| {
                let cx = (rect.left + rect.right) / 2;
                let cy = (rect.top + rect.bottom) / 2;
                let rel_angle = relative_angle(origin_x, origin_y, cx, cy, base_angle);
                FanItemLayout {
                    index,
                    rect,
                    center_x: cx,
                    center_y: cy,
                    rel_angle,
                }
            })
            .collect();

        let shadow_pad = dip_to_px(WINDOW_SHADOW_PAD_DIP, dpi);
        let origin_extent = deadzone_radius + dip_to_px(10, dpi);
        let mut left = origin_x - origin_extent;
        let mut top = origin_y - origin_extent;
        let mut right = origin_x + origin_extent;
        let mut bottom = origin_y + origin_extent;
        for item in &items {
            left = left.min(item.rect.left);
            top = top.min(item.rect.top);
            right = right.max(item.rect.right);
            bottom = bottom.max(item.rect.bottom);
        }
        let bounds = RectData {
            left: left - shadow_pad,
            top: top - shadow_pad,
            right: right + shadow_pad,
            bottom: bottom + shadow_pad,
        };

        Self {
            origin_x,
            origin_y,
            deadzone_radius,
            base_angle,
            is_list_fallback,
            bounds,
            items,
        }
    }

    /// Polar/fan hit-testing used while dragging (`Fanning`): works across the full screen
    /// in the opening hemisphere so quick directional flicks always select cleanly.
    pub fn hit_fanning(&self, x: i32, y: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let dx = f64::from(x - self.origin_x);
        let dy = f64::from(y - self.origin_y);
        let dz = f64::from(self.deadzone_radius);
        if dx * dx + dy * dy < dz * dz {
            return None;
        }
        if let Some(item) = self.items.iter().find(|item| point_in_rect(x, y, &item.rect)) {
            return Some(item.index);
        }
        if self.is_list_fallback {
            return self.hit_list_band(x, y);
        }
        self.hit_by_angle(dx, dy)
    }

    /// Bounded hit-testing used in `Sticky` (click-to-select) mode:
    /// requires the pointer to be near a pill or within the fan bounds (outside the deadzone).
    pub fn hit_sticky(&self, x: i32, y: i32, pad_px: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let dx = f64::from(x - self.origin_x);
        let dy = f64::from(y - self.origin_y);
        let dz = f64::from(self.deadzone_radius);
        if dx * dx + dy * dy < dz * dz {
            return None;
        }
        if let Some(item) = self.items.iter().find(|item| {
            let expanded = RectData {
                left: item.rect.left - pad_px,
                top: item.rect.top - pad_px,
                right: item.rect.right + pad_px,
                bottom: item.rect.bottom + pad_px,
            };
            point_in_rect(x, y, &expanded)
        }) {
            return Some(item.index);
        }
        if !point_in_rect(x, y, &self.bounds) {
            return None;
        }
        if self.is_list_fallback {
            return self.hit_list_band(x, y);
        }
        self.hit_by_angle(dx, dy)
    }

    fn hit_by_angle(&self, dx: f64, dy: f64) -> Option<usize> {
        let rel = rotate_relative(dx, dy, self.base_angle);
        let min_a = self
            .items
            .iter()
            .map(|i| i.rel_angle)
            .fold(f64::INFINITY, f64::min);
        let max_a = self
            .items
            .iter()
            .map(|i| i.rel_angle)
            .fold(f64::NEG_INFINITY, f64::max);

        // Generous cone past edge items (up to ~115 deg from base_angle) so horizontal flicks hit edge items,
        // while pulling backward past ~115 deg cancels safely.
        let margin = 45.0_f64.to_radians();
        let max_cone = 115.0_f64.to_radians();
        let lower = (min_a - margin).max(-max_cone);
        let upper = (max_a + margin).min(max_cone);
        if rel < lower || rel > upper {
            return None;
        }

        self.items
            .iter()
            .min_by(|a, b| {
                let da = (rel - a.rel_angle).abs();
                let db = (rel - b.rel_angle).abs();
                da.total_cmp(&db)
            })
            .map(|item| item.index)
    }

    fn hit_list_band(&self, x: i32, y: i32) -> Option<usize> {
        let list_left = self.items.iter().map(|i| i.rect.left).min()?;
        let list_right = self.items.iter().map(|i| i.rect.right).max()?;
        let list_top = self.items.iter().map(|i| i.rect.top).min()?;
        let list_bottom = self.items.iter().map(|i| i.rect.bottom).max()?;
        if x < list_left || x >= list_right || y < list_top || y >= list_bottom {
            return None;
        }
        self.items
            .iter()
            .min_by_key(|item| (y - item.center_y).abs())
            .map(|item| item.index)
    }
}

fn rotate_relative(dx: f64, dy: f64, base_angle: f64) -> f64 {
    let cos_b = base_angle.cos();
    let sin_b = base_angle.sin();
    let ux = dx * cos_b + dy * sin_b;
    let uy = -dx * sin_b + dy * cos_b;
    uy.atan2(ux)
}

fn relative_angle(origin_x: i32, origin_y: i32, x: i32, y: i32, base_angle: f64) -> f64 {
    rotate_relative(f64::from(x - origin_x), f64::from(y - origin_y), base_angle)
}

fn fan_relative_angles(count: usize) -> Vec<f64> {
    let deg: &[f64] = match count {
        0 => &[],
        1 => &[0.0],
        2 => &[-34.0, 34.0],
        3 => &[-48.0, 0.0, 48.0],
        4 => &[-63.0, -21.0, 21.0, 63.0],
        _ => &[-72.0, -36.0, 0.0, 36.0, 72.0],
    };
    deg.iter().map(|d| d.to_radians()).collect()
}

#[allow(clippy::too_many_arguments)]
fn layout_arc(
    origin_x: i32,
    origin_y: i32,
    deadzone_radius: i32,
    base_angle: f64,
    axis: FanAxis,
    pill_widths: &[i32],
    pill_h: i32,
    gap: i32,
    dpi: u32,
) -> Vec<RectData> {
    let count = pill_widths.len();
    let rel_angles = fan_relative_angles(count);
    let r_base = f64::from(dip_to_px(FAN_RADIUS_DIP, dpi));
    let max_pill_w = *pill_widths.iter().max().unwrap_or(&0);

    // Map logical left-to-right / top-to-bottom ordering to screen angles.
    let screen_angles: Vec<f64> = rel_angles
        .iter()
        .map(|&rel| {
            let signed = match axis {
                FanAxis::Vertical if base_angle > 0.0 => -rel, // Down: index 0 on left
                FanAxis::Horizontal if base_angle.abs() > 1.0 => -rel, // Left: index 0 on top
                _ => rel,
            };
            base_angle + signed
        })
        .collect();

    // Start with an elliptical spread suited to pill aspect ratios (wide horizontally).
    let (rx, ry) = match axis {
        FanAxis::Vertical => {
            let spread_rx = if count >= 4 {
                let min_dx_factor = screen_angles
                    .windows(2)
                    .map(|w| (w[1].cos() - w[0].cos()).abs())
                    .fold(f64::INFINITY, f64::min)
                    .max(0.35);
                let needed = f64::from(max_pill_w + gap) / min_dx_factor;
                needed.clamp(r_base, f64::from(dip_to_px(138, dpi)))
            } else {
                (r_base * 1.08).max(f64::from(max_pill_w) * 0.62)
            };
            (spread_rx, r_base)
        }
        FanAxis::Horizontal => {
            let spread_rx = r_base + f64::from(max_pill_w) * 0.32;
            let spread_ry = if count >= 5 { r_base * 1.15 } else { r_base };
            (spread_rx, spread_ry)
        }
    };

    let mut centers: Vec<(f64, f64, f64)> = screen_angles
        .iter()
        .map(|&ang| {
            let dx = rx * ang.cos();
            let dy = ry * ang.sin();
            (dx, dy, ang)
        })
        .collect();

    // Guarantee every pill clears the center deadzone circle.
    let min_clear_r = f64::from(deadzone_radius + gap);
    for (idx, (dx, dy, ang)) in centers.iter_mut().enumerate() {
        let half_w = f64::from(pill_widths[idx]) / 2.0;
        let half_h = f64::from(pill_h) / 2.0;
        // Closest point on centered rect to origin has distance max(|dx|-half_w, 0), max(|dy|-half_h, 0).
        for _ in 0..16 {
            let cx_gap = (dx.abs() - half_w).max(0.0);
            let cy_gap = (dy.abs() - half_h).max(0.0);
            if cx_gap * cx_gap + cy_gap * cy_gap >= min_clear_r * min_clear_r {
                break;
            }
            let step = f64::from(dip_to_px(6, dpi));
            *dx += ang.cos() * step;
            *dy += ang.sin() * step;
        }
    }

    // Iteratively resolve any pill-to-pill overlap while preserving left-to-right / top-to-bottom order.
    let push_step = f64::from(dip_to_px(6, dpi));
    for _ in 0..48 {
        let mut changed = false;
        for i in 0..count {
            for j in (i + 1)..count {
                let r_i = rect_from_center(
                    origin_x + centers[i].0.round() as i32,
                    origin_y + centers[i].1.round() as i32,
                    pill_widths[i],
                    pill_h,
                );
                let r_j = rect_from_center(
                    origin_x + centers[j].0.round() as i32,
                    origin_y + centers[j].1.round() as i32,
                    pill_widths[j],
                    pill_h,
                );
                if rects_overlap_with_gap(&r_i, &r_j, gap) {
                    let dist_i = centers[i].0 * centers[i].0 + centers[i].1 * centers[i].1;
                    let dist_j = centers[j].0 * centers[j].0 + centers[j].1 * centers[j].1;
                    if (dist_i - dist_j).abs() < 4.0 {
                        // Symmetric pair (e.g. inner pair of 4 items): spread horizontally/vertically
                        // along the tangential axis so symmetry is preserved.
                        match axis {
                            FanAxis::Vertical => {
                                centers[i].0 += centers[i].0.signum() * push_step;
                                centers[j].0 += centers[j].0.signum() * push_step;
                            }
                            FanAxis::Horizontal => {
                                centers[i].1 += centers[i].1.signum() * push_step;
                                centers[j].1 += centers[j].1.signum() * push_step;
                            }
                        }
                    } else {
                        // Push whichever pill is already slightly farther out along its own spoke.
                        let target = if dist_i >= dist_j { i } else { j };
                        let ang = centers[target].2;
                        centers[target].0 += ang.cos() * push_step;
                        centers[target].1 += ang.sin() * push_step;
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    centers
        .into_iter()
        .enumerate()
        .map(|(idx, (dx, dy, _))| {
            rect_from_center(
                origin_x + dx.round() as i32,
                origin_y + dy.round() as i32,
                pill_widths[idx],
                pill_h,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn layout_list(
    origin_x: i32,
    origin_y: i32,
    deadzone_radius: i32,
    base_angle: f64,
    axis: FanAxis,
    pill_widths: &[i32],
    pill_h: i32,
    gap: i32,
    work_area: RectData,
    edge_margin: i32,
) -> Vec<RectData> {
    let count = pill_widths.len();
    let max_w = *pill_widths.iter().max().unwrap_or(&0);
    let step_y = pill_h + gap;
    let total_h = count as i32 * pill_h + (count.saturating_sub(1) as i32) * gap;
    let avail_h = (work_area.height() - edge_margin * 2).max(pill_h);

    // If single column doesn't fit vertically, use 2 columns.
    let columns = if total_h > avail_h && count >= 2 { 2 } else { 1 };
    let rows = count.div_ceil(columns);
    let col_h = rows as i32 * pill_h + (rows.saturating_sub(1) as i32) * gap;
    let total_w = columns as i32 * max_w + (columns as i32 - 1) * gap;

    let (base_left, base_top) = match axis {
        FanAxis::Vertical => {
            let left = origin_x - total_w / 2;
            let top = if base_angle < 0.0 {
                origin_y - deadzone_radius - gap - col_h
            } else {
                origin_y + deadzone_radius + gap
            };
            (left, top)
        }
        FanAxis::Horizontal => {
            let left = if base_angle.abs() < 1.0 {
                origin_x + deadzone_radius + gap
            } else {
                origin_x - deadzone_radius - gap - total_w
            };
            let top = origin_y - col_h / 2;
            (left, top)
        }
    };

    (0..count)
        .map(|idx| {
            let col = idx / rows;
            let row = idx % rows;
            let left = base_left + col as i32 * (max_w + gap);
            let top = base_top + row as i32 * step_y;
            RectData {
                left,
                top,
                right: left + max_w,
                bottom: top + pill_h,
            }
        })
        .collect()
}

fn rect_from_center(cx: i32, cy: i32, w: i32, h: i32) -> RectData {
    let left = cx - w / 2;
    let top = cy - h / 2;
    RectData {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

fn rects_overlap_with_gap(a: &RectData, b: &RectData, gap: i32) -> bool {
    a.left < b.right + gap
        && a.right + gap > b.left
        && a.top < b.bottom + gap
        && a.bottom + gap > b.top
}

fn clamp_rects_to_work_area(rects: &mut [RectData], work_area: RectData, margin: i32) {
    if rects.is_empty() {
        return;
    }
    let min_x = work_area.left + margin;
    let max_x = (work_area.right - margin).max(min_x + 1);
    let min_y = work_area.top + margin;
    let max_y = (work_area.bottom - margin).max(min_y + 1);

    let grp_left = rects.iter().map(|r| r.left).min().unwrap_or(0);
    let grp_right = rects.iter().map(|r| r.right).max().unwrap_or(0);
    let grp_top = rects.iter().map(|r| r.top).min().unwrap_or(0);
    let grp_bottom = rects.iter().map(|r| r.bottom).max().unwrap_or(0);

    let dx = if grp_right - grp_left <= max_x - min_x {
        if grp_left < min_x {
            min_x - grp_left
        } else if grp_right > max_x {
            max_x - grp_right
        } else {
            0
        }
    } else {
        0
    };
    let dy = if grp_bottom - grp_top <= max_y - min_y {
        if grp_top < min_y {
            min_y - grp_top
        } else if grp_bottom > max_y {
            max_y - grp_bottom
        } else {
            0
        }
    } else {
        0
    };

    for r in rects.iter_mut() {
        r.left += dx;
        r.right += dx;
        r.top += dy;
        r.bottom += dy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_1080p() -> RectData {
        RectData {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }

    fn assert_no_overlaps_and_in_bounds(layout: &FanLayout, work_area: RectData) {
        for (i, a) in layout.items.iter().enumerate() {
            assert!(
                a.rect.left >= work_area.left && a.rect.right <= work_area.right,
                "item {i} x out of work_area: {:?} vs {:?}",
                a.rect,
                work_area
            );
            assert!(
                a.rect.top >= work_area.top && a.rect.bottom <= work_area.bottom,
                "item {i} y out of work_area: {:?} vs {:?}",
                a.rect,
                work_area
            );
            for (j, b) in layout.items.iter().enumerate().skip(i + 1) {
                assert!(
                    !rects_overlap_with_gap(&a.rect, &b.rect, 1),
                    "items {i} and {j} overlap: {:?} vs {:?}",
                    a.rect,
                    b.rect
                );
            }
        }
    }

    #[test]
    fn opens_upward_near_bottom_and_downward_near_top() {
        let work = work_1080p();
        let bottom_anchor = RectData {
            left: 900,
            top: 990,
            right: 932,
            bottom: 1022,
        };
        let up = FanLayout::compute(bottom_anchor, &[60, 70, 65], work, FanAxis::Vertical, 96);
        assert!(up.base_angle < 0.0);
        assert!(up.items.iter().all(|i| i.center_y < up.origin_y));
        assert_no_overlaps_and_in_bounds(&up, work);

        let top_anchor = RectData {
            left: 900,
            top: 12,
            right: 932,
            bottom: 44,
        };
        let down = FanLayout::compute(top_anchor, &[60, 70, 65], work, FanAxis::Vertical, 96);
        assert!(down.base_angle > 0.0);
        assert!(down.items.iter().all(|i| i.center_y > down.origin_y));
        assert!(down.items[0].center_x < down.items[1].center_x);
        assert!(down.items[1].center_x < down.items[2].center_x);
        assert_no_overlaps_and_in_bounds(&down, work);
    }

    #[test]
    fn vertical_bar_opens_toward_interior_left_or_right() {
        let work = work_1080p();
        let left_edge = RectData {
            left: 8,
            top: 500,
            right: 40,
            bottom: 532,
        };
        let right = FanLayout::compute(left_edge, &[80, 90, 75], work, FanAxis::Horizontal, 96);
        assert!(right.base_angle.abs() < 0.01);
        assert!(right.items.iter().all(|i| i.center_x > right.origin_x));
        assert!(right.items[0].center_y < right.items[1].center_y);
        assert_no_overlaps_and_in_bounds(&right, work);

        let right_edge = RectData {
            left: 1875,
            top: 500,
            right: 1907,
            bottom: 532,
        };
        let left = FanLayout::compute(right_edge, &[80, 90, 75], work, FanAxis::Horizontal, 96);
        assert!((left.base_angle - PI).abs() < 0.01);
        assert!(left.items.iter().all(|i| i.center_x < left.origin_x));
        assert!(left.items[0].center_y < left.items[1].center_y);
        assert_no_overlaps_and_in_bounds(&left, work);
    }

    #[test]
    fn no_overlaps_for_2_to_5_items_with_long_labels_and_high_dpi() {
        let work = RectData {
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1380,
        };
        let anchor = RectData {
            left: -1300,
            top: 1300,
            right: -1252,
            bottom: 1348,
        };
        for count in 2..=5 {
            let widths = vec![140; count];
            for dpi in [96, 144, 192] {
                let layout = FanLayout::compute(anchor, &widths, work, FanAxis::Vertical, dpi);
                assert!(!layout.is_list_fallback);
                assert_no_overlaps_and_in_bounds(&layout, work);
                for item in &layout.items {
                    assert_eq!(
                        layout.hit_fanning(item.center_x, item.center_y),
                        Some(item.index)
                    );
                }
            }
        }
    }

    #[test]
    fn six_or_more_items_use_list_fallback_and_stay_in_work_area() {
        let work = work_1080p();
        let anchor = RectData {
            left: 20,
            top: 990,
            right: 52,
            bottom: 1022,
        };
        let widths = vec![90; 7];
        let layout = FanLayout::compute(anchor, &widths, work, FanAxis::Vertical, 96);
        assert!(layout.is_list_fallback);
        assert_eq!(layout.items.len(), 7);
        assert_no_overlaps_and_in_bounds(&layout, work);
    }

    #[test]
    fn polar_hit_testing_matches_directions_and_respects_deadzone_and_back_cancel() {
        let work = work_1080p();
        let anchor = RectData {
            left: 900,
            top: 900,
            right: 932,
            bottom: 932,
        };
        let layout = FanLayout::compute(anchor, &[70, 70, 70], work, FanAxis::Vertical, 96);

        // Inside deadzone cancels
        assert_eq!(layout.hit_fanning(layout.origin_x, layout.origin_y), None);
        assert_eq!(layout.hit_fanning(layout.origin_x + 5, layout.origin_y - 5), None);

        // Directional flicks far beyond the pills select cleanly
        assert_eq!(layout.hit_fanning(layout.origin_x - 200, layout.origin_y - 120), Some(0));
        assert_eq!(layout.hit_fanning(layout.origin_x, layout.origin_y - 250), Some(1));
        assert_eq!(layout.hit_fanning(layout.origin_x + 200, layout.origin_y - 120), Some(2));

        // Pulling straight backward cancels
        assert_eq!(layout.hit_fanning(layout.origin_x, layout.origin_y + 80), None);

        // Sticky mode does not highlight far outside bounds
        assert_eq!(layout.hit_sticky(layout.origin_x, layout.origin_y - 400, 4), None);
        assert_eq!(
            layout.hit_sticky(layout.items[1].center_x, layout.items[1].center_y, 4),
            Some(1)
        );
    }
}
