//! Launcher layout geometry and hit-testing logic (`input.rs`).
//!
//! Computes exact physical pixel rectangles for:
//! - Dedicated drag grip (`18 DIP` wide in horizontal, `18 DIP` tall in vertical)
//! - Pinned scene buttons (`32 × 32 DIP` each)
//! - Optional `⋯` overflow button (`32 × 32 DIP`, shown only when `overflow_scenes` is non-empty)
//! - Outer padding (`3 DIP`) and element gap (`2 DIP`), yielding `126 × 38 DIP` for 3 buttons in horizontal mode.

use crate::config::{LauncherConfig, Orientation, RectData, Scene};
use crate::platform::monitors::dip_to_px;
use windows::Win32::UI::WindowsAndMessaging::WHEEL_DELTA;

pub const OUTER_PADDING_DIP: i32 = 3;
pub const ELEMENT_GAP_DIP: i32 = 2;
pub const CORNER_RADIUS_DIP: i32 = 6;
pub const SCENE_VARIANT_BUTTON_DIP: i32 = 20;

#[derive(Default)]
pub(super) struct SceneWheel {
    remainder: i32,
}

impl SceneWheel {
    pub(super) fn steps(&mut self, delta: i16) -> i32 {
        let notch = WHEEL_DELTA as i32;
        let total = self.remainder + i32::from(delta);
        self.remainder = total % notch;
        -(total / notch)
    }
}

pub(super) fn wheel_scene_index(
    scene_count: usize,
    active_index: Option<usize>,
    steps: i32,
    wrap: bool,
) -> Option<usize> {
    if scene_count == 0 || steps == 0 {
        return None;
    }
    let scene_count = scene_count as i64;
    let start = match active_index {
        Some(index) => index as i64,
        None if steps > 0 => -1,
        None => scene_count,
    };
    let next = start + i64::from(steps);
    let next = if wrap {
        next.rem_euclid(scene_count)
    } else {
        next.clamp(0, scene_count - 1)
    } as usize;
    (Some(next) != active_index).then_some(next)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Background / padding (no action).
    None,
    /// Dedicated drag grip (`HTCAPTION`).
    Grip,
    /// A pinned scene button on the main bar.
    SceneButton { index: usize, scene_id: String },
    /// The arrow beside a scene with multiple variants.
    SceneVariants { index: usize, scene_id: String },
    /// The `⋯` overflow button.
    OverflowButton,
}

#[derive(Debug, Clone)]
pub struct ButtonSlotLayout {
    pub rect: RectData,
    pub variants_rect: Option<RectData>,
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

        let variant_size = dip_to_px(SCENE_VARIANT_BUTTON_DIP, dpi);

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
                    let variants_rect = (scene.variants.len() > 1).then(|| {
                        let arrow = RectData {
                            left: cursor_x,
                            right: cursor_x + variant_size,
                            ..rect
                        };
                        cursor_x += variant_size;
                        arrow
                    });
                    scene_buttons.push(ButtonSlotLayout {
                        rect,
                        variants_rect,
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
                let arrow_width = if visible_scenes.iter().any(|scene| scene.variants.len() > 1) {
                    variant_size
                } else {
                    0
                };
                let full_width = btn_size + arrow_width;
                let grip_rect = RectData {
                    left: pad,
                    top: cursor_y,
                    right: pad + full_width,
                    bottom: cursor_y + grip_len,
                };
                cursor_y += grip_len;

                let mut scene_buttons = Vec::with_capacity(visible_scenes.len());
                for scene in visible_scenes {
                    cursor_y += gap;
                    let has_variants = scene.variants.len() > 1;
                    let rect = RectData {
                        left: pad,
                        top: cursor_y,
                        right: pad + if has_variants { btn_size } else { full_width },
                        bottom: cursor_y + btn_size,
                    };
                    cursor_y += btn_size;
                    let variants_rect = has_variants.then_some(RectData {
                        left: rect.right,
                        right: rect.right + variant_size,
                        ..rect
                    });
                    scene_buttons.push(ButtonSlotLayout {
                        rect,
                        variants_rect,
                        scene: scene.clone(),
                    });
                }

                let overflow_button_rect = if has_overflow {
                    cursor_y += gap;
                    let rect = RectData {
                        left: pad,
                        top: cursor_y,
                        right: pad + full_width,
                        bottom: cursor_y + btn_size,
                    };
                    cursor_y += btn_size;
                    Some(rect)
                } else {
                    None
                };

                let width_px = pad + full_width + pad;
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
            if slot
                .variants_rect
                .is_some_and(|rect| point_in_rect(client_x, client_y, &rect))
            {
                return HitTarget::SceneVariants {
                    index,
                    scene_id: slot.scene.id.clone(),
                };
            }
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
    fn wheel_accumulates_partial_deltas_in_both_directions() {
        let mut wheel = SceneWheel::default();
        assert_eq!(wheel.steps(-30), 0);
        assert_eq!(wheel.steps(-60), 0);
        assert_eq!(wheel.steps(30), 0);
        assert_eq!(wheel.steps(-60), 1);
        assert_eq!(wheel.steps(270), -2);
        assert_eq!(wheel.steps(90), -1);
    }

    #[test]
    fn wheel_wraps_around_the_scene_order() {
        assert_eq!(wheel_scene_index(3, Some(0), -1, true), Some(2));
        assert_eq!(wheel_scene_index(3, Some(2), 1, true), Some(0));
        assert_eq!(wheel_scene_index(3, Some(0), 2, true), Some(2));
        assert_eq!(wheel_scene_index(3, Some(0), -3, true), None);
        assert_eq!(wheel_scene_index(3, None, 1, true), Some(0));
        assert_eq!(wheel_scene_index(3, None, -1, true), Some(2));
        assert_eq!(wheel_scene_index(0, None, 1, true), None);
        assert_eq!(wheel_scene_index(1, Some(0), 1, true), None);
    }

    #[test]
    fn wheel_stops_at_the_ends_of_the_scene_order() {
        assert_eq!(wheel_scene_index(3, Some(1), -4, false), Some(0));
        assert_eq!(wheel_scene_index(3, Some(1), 4, false), Some(2));
        assert_eq!(wheel_scene_index(3, Some(0), -1, false), None);
        assert_eq!(wheel_scene_index(3, Some(2), 1, false), None);
        assert_eq!(wheel_scene_index(3, Some(0), 1, false), Some(1));
        assert_eq!(wheel_scene_index(3, Some(2), -1, false), Some(1));
        assert_eq!(wheel_scene_index(3, None, 1, false), Some(0));
        assert_eq!(wheel_scene_index(3, None, -1, false), Some(2));
        assert_eq!(wheel_scene_index(0, None, 1, false), None);
        assert_eq!(wheel_scene_index(1, Some(0), 1, false), None);
    }

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
    fn variant_arrow_is_a_separate_target_in_both_orientations() {
        let mut config = crate::config::AppConfig::default_initial();
        config.scenes[1].variants = ["Antigravity", "Codex"]
            .into_iter()
            .map(|name| crate::config::SceneVariant {
                id: name.into(),
                name: name.into(),
            })
            .collect();
        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            config.launcher.orientation = orientation;
            let layout = LauncherLayout::compute(&config.launcher, &config.scenes, false, 144);
            let slot = &layout.scene_buttons[1];
            let arrow = slot.variants_rect.unwrap();
            assert!(matches!(
                layout.hit_test(slot.rect.left, slot.rect.top),
                HitTarget::SceneButton { index: 1, .. }
            ));
            assert!(matches!(
                layout.hit_test(arrow.left, arrow.top),
                HitTarget::SceneVariants { index: 1, .. }
            ));
            assert!(layout.scene_buttons[0].variants_rect.is_none());
        }
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
