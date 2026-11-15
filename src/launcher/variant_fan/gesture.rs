//! Interaction state machine for press-to-fan / flick / sticky composition switching.

use super::geometry::FanLayout;

pub const HOLD_OPEN_DELAY_MS: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    Idle,
    Pressed,
    Fanning,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureEvent {
    None,
    OpenFan { selected: Option<usize> },
    SelectionChanged { selected: Option<usize> },
    EnterSticky { selected: Option<usize> },
    Commit { variant_index: usize },
    ClickDefault,
    Cancel,
}

#[derive(Debug, Clone)]
enum InternalState {
    Idle,
    Pressed {
        press_x: i32,
        press_y: i32,
        drag_dx: i32,
        drag_dy: i32,
    },
    Fanning {
        opened_by_hold: bool,
        has_left_deadzone: bool,
        selected: Option<usize>,
    },
    Sticky {
        selected: Option<usize>,
        pressed_item: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub struct VariantGesture {
    state: InternalState,
    sticky_pad_px: i32,
}

impl Default for VariantGesture {
    fn default() -> Self {
        Self::new(4)
    }
}

impl VariantGesture {
    pub fn new(sticky_pad_px: i32) -> Self {
        Self {
            state: InternalState::Idle,
            sticky_pad_px,
        }
    }

    pub fn phase(&self) -> GesturePhase {
        match self.state {
            InternalState::Idle => GesturePhase::Idle,
            InternalState::Pressed { .. } => GesturePhase::Pressed,
            InternalState::Fanning { .. } => GesturePhase::Fanning,
            InternalState::Sticky { .. } => GesturePhase::Sticky,
        }
    }

    pub fn is_fan_visible(&self) -> bool {
        matches!(
            self.state,
            InternalState::Fanning { .. } | InternalState::Sticky { .. }
        )
    }

    pub fn selected_index(&self) -> Option<usize> {
        match self.state {
            InternalState::Fanning { selected, .. } | InternalState::Sticky { selected, .. } => {
                selected
            }
            _ => None,
        }
    }

    /// Start tracking a left mouse press on a scene button.
    pub fn begin_press(&mut self, x: i32, y: i32, drag_dx: i32, drag_dy: i32, sticky_pad_px: i32) {
        self.sticky_pad_px = sticky_pad_px;
        self.state = InternalState::Pressed {
            press_x: x,
            press_y: y,
            drag_dx: drag_dx.max(4),
            drag_dy: drag_dy.max(4),
        };
    }

    /// Directly enter `Sticky` mode (e.g. keyboard `Space` activation).
    pub fn open_sticky(&mut self, initial_selected: Option<usize>, sticky_pad_px: i32) {
        self.sticky_pad_px = sticky_pad_px;
        self.state = InternalState::Sticky {
            selected: initial_selected,
            pressed_item: None,
        };
    }

    pub fn on_hold_timer(&mut self, x: i32, y: i32, layout: &FanLayout) -> GestureEvent {
        if !matches!(self.state, InternalState::Pressed { .. }) {
            return GestureEvent::None;
        }
        let selected = layout.hit_fanning(x, y);
        self.state = InternalState::Fanning {
            opened_by_hold: true,
            has_left_deadzone: selected.is_some(),
            selected,
        };
        GestureEvent::OpenFan { selected }
    }

    pub fn on_move(&mut self, x: i32, y: i32, layout: Option<&FanLayout>) -> GestureEvent {
        match self.state {
            InternalState::Idle => GestureEvent::None,
            InternalState::Pressed {
                press_x,
                press_y,
                drag_dx,
                drag_dy,
            } => {
                if (x - press_x).abs() <= drag_dx && (y - press_y).abs() <= drag_dy {
                    return GestureEvent::None;
                }
                let selected = layout.and_then(|l| l.hit_fanning(x, y));
                self.state = InternalState::Fanning {
                    opened_by_hold: false,
                    has_left_deadzone: selected.is_some(),
                    selected,
                };
                GestureEvent::OpenFan { selected }
            }
            InternalState::Fanning {
                opened_by_hold,
                has_left_deadzone,
                selected: prev,
            } => {
                let next = layout.and_then(|l| l.hit_fanning(x, y));
                let left = has_left_deadzone || next.is_some();
                self.state = InternalState::Fanning {
                    opened_by_hold,
                    has_left_deadzone: left,
                    selected: next,
                };
                if next != prev {
                    GestureEvent::SelectionChanged { selected: next }
                } else {
                    GestureEvent::None
                }
            }
            InternalState::Sticky {
                selected: prev,
                pressed_item,
            } => {
                let next = layout.and_then(|l| l.hit_sticky(x, y, self.sticky_pad_px));
                if next != prev {
                    self.state = InternalState::Sticky {
                        selected: next,
                        pressed_item,
                    };
                    GestureEvent::SelectionChanged { selected: next }
                } else {
                    GestureEvent::None
                }
            }
        }
    }

    pub fn on_down(&mut self, x: i32, y: i32, layout: &FanLayout) -> GestureEvent {
        let InternalState::Sticky { .. } = self.state else {
            return GestureEvent::None;
        };
        match layout.hit_sticky(x, y, self.sticky_pad_px) {
            Some(idx) => {
                self.state = InternalState::Sticky {
                    selected: Some(idx),
                    pressed_item: Some(idx),
                };
                GestureEvent::SelectionChanged {
                    selected: Some(idx),
                }
            }
            None => {
                self.state = InternalState::Idle;
                GestureEvent::Cancel
            }
        }
    }

    pub fn on_up(&mut self, x: i32, y: i32, layout: Option<&FanLayout>) -> GestureEvent {
        match self.state.clone() {
            InternalState::Idle => GestureEvent::None,
            InternalState::Pressed { .. } => {
                self.state = InternalState::Idle;
                GestureEvent::ClickDefault
            }
            InternalState::Fanning {
                opened_by_hold,
                has_left_deadzone,
                selected,
            } => {
                let final_sel = layout.and_then(|l| l.hit_fanning(x, y)).or(selected);
                if let Some(idx) = final_sel {
                    self.state = InternalState::Idle;
                    GestureEvent::Commit { variant_index: idx }
                } else if opened_by_hold && !has_left_deadzone {
                    self.state = InternalState::Sticky {
                        selected: None,
                        pressed_item: None,
                    };
                    GestureEvent::EnterSticky { selected: None }
                } else {
                    self.state = InternalState::Idle;
                    GestureEvent::Cancel
                }
            }
            InternalState::Sticky {
                pressed_item: Some(p_idx),
                ..
            } => {
                let hit = layout.and_then(|l| l.hit_sticky(x, y, self.sticky_pad_px));
                if hit == Some(p_idx) {
                    self.state = InternalState::Idle;
                    GestureEvent::Commit {
                        variant_index: p_idx,
                    }
                } else {
                    self.state = InternalState::Sticky {
                        selected: hit,
                        pressed_item: None,
                    };
                    GestureEvent::SelectionChanged { selected: hit }
                }
            }
            InternalState::Sticky {
                pressed_item: None,
                ..
            } => GestureEvent::None,
        }
    }

    pub fn on_cancel(&mut self) -> GestureEvent {
        if matches!(self.state, InternalState::Idle) {
            return GestureEvent::None;
        }
        self.state = InternalState::Idle;
        GestureEvent::Cancel
    }

    pub fn step_sticky_selection(&mut self, delta: i32, count: usize) -> GestureEvent {
        let InternalState::Sticky { selected, .. } = self.state else {
            return GestureEvent::None;
        };
        if count == 0 {
            return GestureEvent::None;
        }
        let next = match selected {
            Some(cur) => ((cur as i32 + delta).rem_euclid(count as i32)) as usize,
            None if delta >= 0 => 0,
            None => count - 1,
        };
        self.state = InternalState::Sticky {
            selected: Some(next),
            pressed_item: None,
        };
        GestureEvent::SelectionChanged {
            selected: Some(next),
        }
    }

    pub fn set_sticky_selection(&mut self, index: usize, count: usize) -> GestureEvent {
        let InternalState::Sticky { .. } = self.state else {
            return GestureEvent::None;
        };
        if index >= count {
            return GestureEvent::None;
        }
        self.state = InternalState::Sticky {
            selected: Some(index),
            pressed_item: None,
        };
        GestureEvent::SelectionChanged {
            selected: Some(index),
        }
    }

    pub fn confirm_sticky(&mut self) -> GestureEvent {
        let InternalState::Sticky { selected, .. } = self.state else {
            return GestureEvent::None;
        };
        self.state = InternalState::Idle;
        match selected {
            Some(variant_index) => GestureEvent::Commit { variant_index },
            None => GestureEvent::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RectData;
    use crate::launcher::variant_fan::geometry::FanAxis;

    fn sample_layout() -> FanLayout {
        FanLayout::compute(
            RectData {
                left: 900,
                top: 900,
                right: 932,
                bottom: 932,
            },
            &[70, 70, 70],
            RectData {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
            FanAxis::Vertical,
            96,
        )
    }

    #[test]
    fn quick_click_commits_default() {
        let layout = sample_layout();
        let mut g = VariantGesture::default();
        g.begin_press(layout.origin_x, layout.origin_y, 4, 4, 4);
        assert_eq!(g.on_move(layout.origin_x + 2, layout.origin_y - 2, Some(&layout)), GestureEvent::None);
        assert_eq!(g.on_up(layout.origin_x + 2, layout.origin_y - 2, Some(&layout)), GestureEvent::ClickDefault);
        assert_eq!(g.phase(), GesturePhase::Idle);
    }

    #[test]
    fn drag_flick_opens_fan_and_commits_on_release() {
        let layout = sample_layout();
        let mut g = VariantGesture::default();
        g.begin_press(layout.origin_x, layout.origin_y, 6, 6, 4);
        assert!(matches!(
            g.on_move(layout.origin_x, layout.origin_y - 10, Some(&layout)),
            GestureEvent::OpenFan { selected: None }
        ));
        assert_eq!(g.phase(), GesturePhase::Fanning);
        assert_eq!(
            g.on_move(layout.origin_x, layout.origin_y - 80, Some(&layout)),
            GestureEvent::SelectionChanged { selected: Some(1) }
        );
        assert_eq!(
            g.on_up(layout.origin_x, layout.origin_y - 80, Some(&layout)),
            GestureEvent::Commit { variant_index: 1 }
        );
        assert_eq!(g.phase(), GesturePhase::Idle);
    }

    #[test]
    fn dragging_back_to_center_cancels() {
        let layout = sample_layout();
        let mut g = VariantGesture::default();
        g.begin_press(layout.origin_x, layout.origin_y, 6, 6, 4);
        let _ = g.on_move(layout.origin_x, layout.origin_y - 60, Some(&layout));
        assert_eq!(
            g.on_move(layout.origin_x, layout.origin_y, Some(&layout)),
            GestureEvent::SelectionChanged { selected: None }
        );
        assert_eq!(
            g.on_up(layout.origin_x, layout.origin_y, Some(&layout)),
            GestureEvent::Cancel
        );
    }

    #[test]
    fn hold_and_release_in_center_enters_sticky_for_trackpad_click() {
        let layout = sample_layout();
        let mut g = VariantGesture::default();
        g.begin_press(layout.origin_x, layout.origin_y, 6, 6, 4);
        assert_eq!(
            g.on_hold_timer(layout.origin_x, layout.origin_y, &layout),
            GestureEvent::OpenFan { selected: None }
        );
        assert_eq!(
            g.on_up(layout.origin_x, layout.origin_y, Some(&layout)),
            GestureEvent::EnterSticky { selected: None }
        );
        assert_eq!(g.phase(), GesturePhase::Sticky);

        let target = &layout.items[2];
        assert_eq!(
            g.on_move(target.center_x, target.center_y, Some(&layout)),
            GestureEvent::SelectionChanged { selected: Some(2) }
        );
        assert_eq!(
            g.on_down(target.center_x, target.center_y, &layout),
            GestureEvent::SelectionChanged { selected: Some(2) }
        );
        assert_eq!(
            g.on_up(target.center_x, target.center_y, Some(&layout)),
            GestureEvent::Commit { variant_index: 2 }
        );
        assert_eq!(g.phase(), GesturePhase::Idle);
    }

    #[test]
    fn sticky_outside_click_cancels() {
        let layout = sample_layout();
        let mut g = VariantGesture::default();
        g.open_sticky(None, 4);
        assert_eq!(g.on_down(0, 0, &layout), GestureEvent::Cancel);
        assert_eq!(g.phase(), GesturePhase::Idle);
    }
}
