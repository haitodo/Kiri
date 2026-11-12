//! Launcher layout geometry and hit-testing logic (`input.rs`).
//!
//! Computes exact physical pixel rectangles for:
//! - Dedicated drag grip (`18 DIP` wide in horizontal, `18 DIP` tall in vertical)
//! - Pinned scene buttons (`32 × 32 DIP` each)
//! - Optional `⋯` overflow button (`32 × 32 DIP`, shown only when `overflow_scenes` is non-empty)
//! - Outer padding (`3 DIP`) and element gap (`2 DIP`), yielding `126 × 38 DIP` for 3 buttons in horizontal mode.

use crate::config::{LauncherConfig, Orientation, RectData, Scene};
use crate::platform::monitors::dip_to_px;

pub const OUTER_PADDING_DIP: i32 = 3;
pub const ELEMENT_GAP_DIP: i32 = 2;
pub const CORNER_RADIUS_DIP: i32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Background / padding (no action).
    None,
    /// Dedicated drag grip (`HTCAPTION`).
    Grip,
    /// A pinned scene button on the main bar.
    SceneButton { index: usize, scene_id: String },
    /// The `⋯` overflow button.
    OverflowButton,
}

#[derive(Debug, Clone)]
pub struct ButtonSlotLayout {
    pub rect: RectData,
    pub scene: Scene,
}

#[derive(Debug, Clone)]
pub struct LauncherLayout {
    pub width_px: i32,
    pub height_px: i32,
    pub dpi: u32,
    pub orientation: Orientation,
    pub grip_rect: RectData,
    pub scene_buttons: Vec<ButtonSlotLayout>,
    pub overflow_button_rect: Option<RectData>,
}

impl LauncherLayout {
    /// Compute the complete physical-pixel layout of the launcher for a given `dpi`.
    pub fn compute(
        launcher_cfg: &LauncherConfig,
        visible_scenes: &[Scene],
        has_overflow: bool,
        dpi: u32,
    ) -> Self {
        let pad = dip_to_px(OUTER_PADDING_DIP, dpi);
        let gap = dip_to_px(ELEMENT_GAP_DIP, dpi);
        let btn_size = dip_to_px(launcher_cfg.button_size_dip.max(20), dpi);
        let grip_len = dip_to_px(launcher_cfg.grip_size_dip.max(10), dpi);

        let total_buttons = visible_scenes.len() + usize::from(has_overflow);

        match launcher_cfg.orientation {
            Orientation::Horizontal => {
                let mut cursor_x = pad;
                let grip_rect = RectData {
                    left: cursor_x,
                    top: pad,
                    right: cursor_x + grip_len,
                    bottom: pad + btn_size,
                };
                cursor_x += grip_len;

                let mut scene_buttons = Vec::with_capacity(visible_scenes.len());
                for scene in visible_scenes {
                    cursor_x += gap;
                    let rect = RectData {
                        left: cursor_x,
                        top: pad,
                        right: cursor_x + btn_size,
                        bottom: pad + btn_size,
                    };
                    cursor_x += btn_size;
                    scene_buttons.push(ButtonSlotLayout {
                        rect,
                        scene: scene.clone(),
                    });
                }

                let overflow_button_rect = if has_overflow {
                    cursor_x += gap;
                    let rect = RectData {
                        left: cursor_x,
                        top: pad,
                        right: cursor_x + btn_size,
                        bottom: pad + btn_size,
                    };
                    cursor_x += btn_size;
                    Some(rect)
                } else {
                    None
                };

                let _ = total_buttons;
                let width_px = cursor_x + pad;
                let height_px = pad + btn_size + pad;

                Self {
                    width_px,
                    height_px,
                    dpi,
                    orientation: Orientation::Horizontal,
                    grip_rect,
                    scene_buttons,
                    overflow_button_rect,
                }
            }
            Orientation::Vertical => {
                let mut cursor_y = pad;
                let grip_rect = RectData {
                    left: pad,
                    top: cursor_y,
                    right: pad + btn_size,
                    bottom: cursor_y + grip_len,
                };
                cursor_y += grip_len;

                let mut scene_buttons = Vec::with_capacity(visible_scenes.len());
                for scene in visible_scenes {
                    cursor_y += gap;
                    let rect = RectData {
                        left: pad,
                        top: cursor_y,
                        right: pad + btn_size,
                        bottom: cursor_y + btn_size,
                    };
                    cursor_y += btn_size;
                    scene_buttons.push(ButtonSlotLayout {
                        rect,
                        scene: scene.clone(),
                    });
                }

                let overflow_button_rect = if has_overflow {
                    cursor_y += gap;
                    let rect = RectData {
                        left: pad,
                        top: cursor_y,
                        right: pad + btn_size,
                        bottom: cursor_y + btn_size,
                    };
                    cursor_y += btn_size;
                    Some(rect)
                } else {
                    None
                };

                let width_px = pad + btn_size + pad;
                let height_px = cursor_y + pad;

                Self {
                    width_px,
                    height_px,
                    dpi,
                    orientation: Orientation::Vertical,
                    grip_rect,
                    scene_buttons,
                    overflow_button_rect,
                }
            }
        }
    }

    /// Perform hit-testing in client coordinates `(client_x, client_y)`.
    pub fn hit_test(&self, client_x: i32, client_y: i32) -> HitTarget {
        if point_in_rect(client_x, client_y, &self.grip_rect) {
            return HitTarget::Grip;
        }
        for (index, slot) in self.scene_buttons.iter().enumerate() {
            if point_in_rect(client_x, client_y, &slot.rect) {
                return HitTarget::SceneButton {
                    index,
                    scene_id: slot.scene.id.clone(),
                };
            }
        }
        if let Some(ref ov_rect) = self.overflow_button_rect {
            if point_in_rect(client_x, client_y, ov_rect) {
                return HitTarget::OverflowButton;
            }
        }
        HitTarget::None
    }
}

#[inline]
pub fn point_in_rect(x: i32, y: i32, r: &RectData) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn test_launcher_dimensions_match_spec_126x38_at_96dpi() {
        let cfg = AppConfig::default_initial();
        let (visible, overflow) = cfg.split_visible_and_overflow();
        let layout =
            LauncherLayout::compute(&cfg.launcher, &visible, !overflow.is_empty(), 96);
        assert_eq!(layout.width_px, 126);
        assert_eq!(layout.height_px, 38);

        // Hit-test grip vs first button
        assert_eq!(layout.hit_test(10, 19), HitTarget::Grip);
        assert_eq!(
            layout.hit_test(35, 19),
            HitTarget::SceneButton {
                index: 0,
                scene_id: "trade".to_string()
            }
        );
    }

    #[test]
    fn test_vertical_orientation_dimensions_38x126() {
        let mut cfg = AppConfig::default_initial();
        cfg.launcher.orientation = Orientation::Vertical;
        let (visible, overflow) = cfg.split_visible_and_overflow();
        let layout =
            LauncherLayout::compute(&cfg.launcher, &visible, !overflow.is_empty(), 96);
        assert_eq!(layout.width_px, 38);
        assert_eq!(layout.height_px, 126);
    }
}
