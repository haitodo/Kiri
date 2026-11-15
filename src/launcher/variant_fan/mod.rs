//! Interactive directional fan menu for switching scene variants without stealing focus.

pub mod geometry;
pub mod gesture;
#[cfg(windows)]
mod window;

pub use geometry::{FanAxis, FanLayout};
pub use gesture::{GestureEvent, GesturePhase, VariantGesture, HOLD_OPEN_DELAY_MS};

#[cfg(windows)]
use self::window::VariantFanWindow;
#[cfg(windows)]
use crate::config::{RectData, Scene};
#[cfg(windows)]
use crate::launcher::paint::GdiplusContext;
#[cfg(windows)]
use crate::platform::monitors::dip_to_px;

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanOutcome {
    None,
    Opened,
    Updated,
    EnteredSticky,
    ClickDefault { scene_id: String },
    CommitVariant { scene_id: String, variant_id: String },
    Cancelled,
}

#[cfg(windows)]
struct ActiveFanSession {
    scene: Scene,
    preferred_index: Option<usize>,
    layout: FanLayout,
    dpi: u32,
}

#[cfg(windows)]
pub struct VariantFanHost {
    gdiplus: GdiplusContext,
    window: Option<VariantFanWindow>,
    gesture: VariantGesture,
    session: Option<ActiveFanSession>,
}

#[cfg(windows)]
impl VariantFanHost {
    pub fn new(gdiplus: GdiplusContext) -> Self {
        Self {
            gdiplus,
            window: None,
            gesture: VariantGesture::default(),
            session: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.gesture.phase() != GesturePhase::Idle
    }

    pub fn is_visible(&self) -> bool {
        self.gesture.is_fan_visible()
    }

    pub fn is_sticky(&self) -> bool {
        self.gesture.phase() == GesturePhase::Sticky
    }

    pub fn active_scene_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.scene.id.as_str())
    }

    fn ensure_window(&mut self) -> Option<&mut VariantFanWindow> {
        if self.window.is_none() {
            self.window = VariantFanWindow::create(self.gdiplus.clone()).ok();
        }
        self.window.as_mut()
    }

    fn build_session(
        &mut self,
        scene: &Scene,
        anchor_screen: RectData,
        work_area: RectData,
        axis: FanAxis,
        dpi: u32,
    ) -> Option<ActiveFanSession> {
        if scene.variants.len() < 2 {
            return None;
        }
        let dpi = dpi.max(96);
        let widths = self
            .ensure_window()
            .map(|w| w.measure_label_widths(scene, dpi))
            .unwrap_or_else(|| vec![dip_to_px(72, dpi); scene.variants.len()]);
        let layout = FanLayout::compute(anchor_screen, &widths, work_area, axis, dpi);
        let preferred_index = scene
            .preferred_variant()
            .and_then(|pref| scene.variants.iter().position(|v| v.id == pref.id));
        Some(ActiveFanSession {
            scene: scene.clone(),
            preferred_index,
            layout,
            dpi,
        })
    }

    pub fn begin_press(
        &mut self,
        scene: &Scene,
        anchor_screen: RectData,
        work_area: RectData,
        axis: FanAxis,
        dpi: u32,
        press_x: i32,
        press_y: i32,
    ) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};

        let Some(session) = self.build_session(scene, anchor_screen, work_area, axis, dpi) else {
            return false;
        };
        let (sys_dx, sys_dy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
        let min_drag = dip_to_px(5, dpi.max(96));
        let sticky_pad = dip_to_px(4, dpi.max(96));
        self.session = Some(session);
        self.gesture.begin_press(
            press_x,
            press_y,
            sys_dx.max(min_drag),
            sys_dy.max(min_drag),
            sticky_pad,
        );
        true
    }

    pub fn open_sticky_keyboard(
        &mut self,
        scene: &Scene,
        anchor_screen: RectData,
        work_area: RectData,
        axis: FanAxis,
        dpi: u32,
    ) -> bool {
        let Some(session) = self.build_session(scene, anchor_screen, work_area, axis, dpi) else {
            return false;
        };
        let initial = session.preferred_index.or(Some(0));
        let sticky_pad = dip_to_px(4, dpi.max(96));
        self.session = Some(session);
        self.gesture.open_sticky(initial, sticky_pad);
        self.render_current();
        true
    }

    pub fn on_hold_timer(&mut self, cursor_x: i32, cursor_y: i32) -> FanOutcome {
        let Some(ref session) = self.session else {
            return FanOutcome::None;
        };
        let ev = self.gesture.on_hold_timer(cursor_x, cursor_y, &session.layout);
        self.apply_event(ev)
    }

    pub fn on_move(&mut self, cursor_x: i32, cursor_y: i32) -> FanOutcome {
        let layout = self.session.as_ref().map(|s| &s.layout);
        let ev = self.gesture.on_move(cursor_x, cursor_y, layout);
        self.apply_event(ev)
    }

    pub fn on_down(&mut self, cursor_x: i32, cursor_y: i32) -> FanOutcome {
        let Some(ref session) = self.session else {
            return FanOutcome::None;
        };
        let ev = self.gesture.on_down(cursor_x, cursor_y, &session.layout);
        self.apply_event(ev)
    }

    pub fn on_up(&mut self, cursor_x: i32, cursor_y: i32) -> FanOutcome {
        let layout = self.session.as_ref().map(|s| &s.layout);
        let ev = self.gesture.on_up(cursor_x, cursor_y, layout);
        self.apply_event(ev)
    }

    pub fn on_key(&mut self, vk: u16) -> FanOutcome {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
        };
        if !self.is_active() {
            return FanOutcome::None;
        }
        if vk == VK_ESCAPE.0 {
            return self.cancel();
        }
        if !self.is_sticky() {
            return FanOutcome::None;
        }
        let count = self.session.as_ref().map_or(0, |s| s.scene.variants.len());
        let ev = if vk == VK_LEFT.0 || vk == VK_UP.0 {
            self.gesture.step_sticky_selection(-1, count)
        } else if vk == VK_RIGHT.0 || vk == VK_DOWN.0 {
            self.gesture.step_sticky_selection(1, count)
        } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            self.gesture.confirm_sticky()
        } else if (b'1'..=b'9').contains(&(vk as u8)) {
            let idx = (vk as u8 - b'1') as usize;
            if idx < count {
                let _ = self.gesture.set_sticky_selection(idx, count);
                self.gesture.confirm_sticky()
            } else {
                GestureEvent::None
            }
        } else {
            GestureEvent::None
        };
        self.apply_event(ev)
    }

    pub fn cancel(&mut self) -> FanOutcome {
        let ev = self.gesture.on_cancel();
        self.apply_event(ev)
    }

    fn render_current(&mut self) {
        let selected = self.gesture.selected_index();
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let scene = session.scene.clone();
        let layout = session.layout.clone();
        let preferred = session.preferred_index;
        let dpi = session.dpi;
        if let Some(window) = self.ensure_window() {
            window.render(&scene, &layout, selected, preferred, dpi);
        }
    }

    fn hide_window(&mut self) {
        if let Some(ref mut window) = self.window {
            window.hide();
        }
    }

    fn apply_event(&mut self, ev: GestureEvent) -> FanOutcome {
        match ev {
            GestureEvent::None => FanOutcome::None,
            GestureEvent::OpenFan { .. } => {
                self.render_current();
                FanOutcome::Opened
            }
            GestureEvent::SelectionChanged { .. } => {
                if self.gesture.is_fan_visible() {
                    self.render_current();
                }
                FanOutcome::Updated
            }
            GestureEvent::EnterSticky { .. } => {
                self.render_current();
                FanOutcome::EnteredSticky
            }
            GestureEvent::ClickDefault => {
                self.hide_window();
                let scene_id = self
                    .session
                    .take()
                    .map(|s| s.scene.id)
                    .unwrap_or_default();
                if scene_id.is_empty() {
                    FanOutcome::None
                } else {
                    FanOutcome::ClickDefault { scene_id }
                }
            }
            GestureEvent::Commit { variant_index } => {
                self.hide_window();
                let session = self.session.take();
                if let Some(s) = session {
                    if let Some(variant) = s.scene.variants.get(variant_index) {
                        return FanOutcome::CommitVariant {
                            scene_id: s.scene.id,
                            variant_id: variant.id.clone(),
                        };
                    }
                }
                FanOutcome::Cancelled
            }
            GestureEvent::Cancel => {
                self.hide_window();
                self.session = None;
                FanOutcome::Cancelled
            }
        }
    }
}
