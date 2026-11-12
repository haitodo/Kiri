//! Observe Ctrl taps and outside clicks, and route the wheel to the non-activating launcher.

use crate::config::RectData;
use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LBUTTON, VK_LCONTROL, VK_LWIN, VK_MBUTTON, VK_MENU,
    VK_RBUTTON, VK_RCONTROL, VK_RWIN, VK_SHIFT, VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WindowFromPoint,
    HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_XBUTTONDOWN,
};

pub const WM_APP_TOGGLE_CURSOR_POPUP: u32 = 0x8000 + 7;
pub const WM_APP_DISMISS_CURSOR_POPUP: u32 = 0x8000 + 8;
pub const WM_APP_CURSOR_POPUP_WHEEL: u32 = 0x8000 + 11;
const DOUBLE_TAP_MS: u32 = 450;
const LEFT_CTRL: u8 = 1;
const RIGHT_CTRL: u8 = 2;

#[derive(Clone, Copy)]
pub struct PopupInput {
    pub rect: RectData,
    pub corner_radius: i32,
    pub has_scenes: bool,
}

impl PopupInput {
    fn contains(self, point: POINT) -> bool {
        let rect = self.rect;
        if point.x < rect.left
            || point.x >= rect.right
            || point.y < rect.top
            || point.y >= rect.bottom
        {
            return false;
        }
        let radius = self
            .corner_radius
            .min(rect.width() / 2)
            .min(rect.height() / 2);
        let x = f64::from(point.x) + 0.5;
        let y = f64::from(point.y) + 0.5;
        let dx = x - x.clamp(
            f64::from(rect.left + radius),
            f64::from(rect.right - radius),
        );
        let dy = y - y.clamp(
            f64::from(rect.top + radius),
            f64::from(rect.bottom - radius),
        );
        dx * dx + dy * dy <= f64::from(radius).powi(2)
    }
}

#[derive(Clone, Copy, Default)]
struct CtrlTaps {
    held: u8,
    pressed_at: Option<u32>,
    first_release: Option<u32>,
}

impl CtrlTaps {
    fn interrupt(&mut self) {
        self.pressed_at = None;
        self.first_release = None;
    }

    fn ctrl(&mut self, side: u8, down: bool, time: u32, chord_held: bool) -> bool {
        if down {
            if self.held & side == 0 {
                if self.held != 0 || chord_held {
                    self.interrupt();
                } else {
                    self.pressed_at = Some(time);
                }
                self.held |= side;
            }
            return false;
        }

        if self.held & side == 0 {
            return false;
        }
        self.held &= !side;
        let Some(press) = self.pressed_at.take() else {
            return false;
        };
        if time.wrapping_sub(press) > DOUBLE_TAP_MS {
            self.interrupt();
            return false;
        }
        if self
            .first_release
            .take()
            .is_some_and(|first| time.wrapping_sub(first) <= DOUBLE_TAP_MS)
        {
            return true;
        }
        self.first_release = Some(time);
        false
    }
}

#[derive(Clone, Copy, Default)]
struct InputState {
    hwnd: HWND,
    taps: CtrlTaps,
    popup: Option<PopupInput>,
    bar_hovered: bool,
}

// Both low-level callbacks run on the UI thread that installed them.
thread_local! {
    static STATE: Cell<InputState> = Cell::new(InputState::default());
    static MOUSE_HOOK: Cell<HHOOK> = Cell::new(HHOOK::default());
}

pub struct CursorInput {
    keyboard_hook: Option<HHOOK>,
}

impl CursorInput {
    pub fn new(hwnd: HWND) -> Self {
        STATE.set(InputState {
            hwnd,
            ..InputState::default()
        });
        Self {
            keyboard_hook: None,
        }
    }

    pub fn set_ctrl_enabled(&mut self, enabled: bool) -> windows::core::Result<()> {
        if enabled && self.keyboard_hook.is_none() {
            self.keyboard_hook = Some(unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_proc),
                    GetModuleHandleW(None)?,
                    0,
                )?
            });
        } else if !enabled {
            if let Some(hook) = self.keyboard_hook.take() {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
            self.reset_gesture();
        }
        Ok(())
    }

    pub fn reset_gesture(&self) {
        let mut state = STATE.get();
        state.taps = CtrlTaps::default();
        STATE.set(state);
        let _ = update_mouse_hook(state);
    }

    pub fn set_popup(&self, popup: Option<PopupInput>) -> windows::core::Result<()> {
        let mut state = STATE.get();
        state.popup = popup;
        STATE.set(state);
        update_mouse_hook(state)
    }

    pub fn set_bar_hovered(&self, hovered: bool) -> windows::core::Result<()> {
        let mut state = STATE.get();
        state.bar_hovered = hovered;
        STATE.set(state);
        update_mouse_hook(state)
    }
}

impl Drop for CursorInput {
    fn drop(&mut self) {
        let _ = self.set_ctrl_enabled(false);
        let state = InputState::default();
        STATE.set(state);
        let _ = update_mouse_hook(state);
    }
}

fn update_mouse_hook(state: InputState) -> windows::core::Result<()> {
    let hook = MOUSE_HOOK.get();
    if state.taps.held != 0 || state.popup.is_some() || state.bar_hovered {
        if hook.0.is_null() {
            MOUSE_HOOK.set(unsafe {
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), GetModuleHandleW(None)?, 0)?
            });
        }
    } else if !hook.0.is_null() {
        MOUSE_HOOK.set(HHOOK::default());
        unsafe { UnhookWindowsHookEx(hook)? };
    }
    Ok(())
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let _ = std::panic::catch_unwind(|| {
        if code != HC_ACTION as i32 {
            return;
        }
        let key = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let mut state = STATE.get();
        let side = match key.vkCode as u16 {
            code if code == VK_LCONTROL.0 => LEFT_CTRL,
            code if code == VK_RCONTROL.0 => RIGHT_CTRL,
            code if code == VK_CONTROL.0 && key.flags.contains(LLKHF_EXTENDED) => RIGHT_CTRL,
            code if code == VK_CONTROL.0 => LEFT_CTRL,
            _ => {
                state.taps.interrupt();
                STATE.set(state);
                return;
            }
        };
        let down = !key.flags.contains(LLKHF_UP);
        let was_held = state.taps.held;
        let chord_held = down && was_held == 0 && {
            let other_ctrl = if side == LEFT_CTRL {
                VK_RCONTROL
            } else {
                VK_LCONTROL
            };
            [
                other_ctrl,
                VK_SHIFT,
                VK_MENU,
                VK_LWIN,
                VK_RWIN,
                VK_LBUTTON,
                VK_RBUTTON,
                VK_MBUTTON,
                VK_XBUTTON1,
                VK_XBUTTON2,
            ]
            .iter()
            .any(|vk| GetAsyncKeyState(i32::from(vk.0)) < 0)
        };
        let toggle = state.taps.ctrl(side, down, key.time, chord_held);
        STATE.set(state);
        if (was_held == 0) != (state.taps.held == 0) {
            let _ = update_mouse_hook(state);
        }
        if toggle {
            let _ = PostMessageW(state.hwnd, WM_APP_TOGGLE_CURSOR_POPUP, WPARAM(0), LPARAM(0));
        }
    });
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let message = wparam.0 as u32;
    let button = matches!(
        message,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    );
    if code == HC_ACTION as i32 && (button || matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL)) {
        let handled = std::panic::catch_unwind(|| {
            let mut state = STATE.get();
            state.taps.interrupt();
            STATE.set(state);
            let mouse = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if button && state.popup.is_some_and(|popup| !popup.contains(mouse.pt)) {
                let _ = PostMessageW(
                    state.hwnd,
                    WM_APP_DISMISS_CURSOR_POPUP,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            if message != WM_MOUSEWHEEL {
                return false;
            }

            let target_message = match state.popup {
                Some(popup) if popup.has_scenes || popup.contains(mouse.pt) => {
                    WM_APP_CURSOR_POPUP_WHEEL
                }
                None if state.bar_hovered && WindowFromPoint(mouse.pt) == state.hwnd => {
                    WM_MOUSEWHEEL
                }
                _ => return false,
            };
            // Use the same delta and screen coordinate fields as WM_MOUSEWHEEL.
            let delta = (mouse.mouseData & 0xffff_0000) as usize;
            let x = u32::from(mouse.pt.x as u16);
            let y = u32::from(mouse.pt.y as u16);
            PostMessageW(
                state.hwnd,
                target_message,
                WPARAM(delta),
                LPARAM(((y << 16) | x) as isize),
            )
            .is_ok()
        })
        .unwrap_or(false);
        if handled {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_at_the_invocation_point_routes_only_while_the_scene_popup_is_open() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_REMOVE,
            WINDOW_EX_STYLE, WINDOW_STYLE,
        };

        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                None,
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                GetModuleHandleW(None).unwrap(),
                None,
            )
            .unwrap();
            let input = CursorInput::new(hwnd);
            let mouse = MSLLHOOKSTRUCT {
                pt: POINT { x: -350, y: 175 },
                mouseData: u32::from(-120i16 as u16) << 16,
                ..Default::default()
            };
            let send_wheel = || {
                mouse_proc(
                    HC_ACTION as i32,
                    WPARAM(WM_MOUSEWHEEL as usize),
                    LPARAM(&mouse as *const _ as isize),
                )
            };
            let receive_wheel = |message: &mut MSG| {
                PeekMessageW(
                    message,
                    hwnd,
                    WM_APP_CURSOR_POPUP_WHEEL,
                    WM_APP_CURSOR_POPUP_WHEEL,
                    PM_REMOVE,
                )
                .as_bool()
            };
            let popup = PopupInput {
                rect: RectData {
                    left: -310,
                    top: 100,
                    right: -1,
                    bottom: 358,
                },
                corner_radius: 10,
                has_scenes: true,
            };
            STATE.set(InputState {
                hwnd,
                popup: Some(popup),
                ..InputState::default()
            });
            let mut message = MSG::default();
            assert_eq!(send_wheel().0, 1);
            assert!(receive_wheel(&mut message));
            assert_eq!((message.wParam.0 >> 16) as i16, -120);
            assert_eq!(message.lParam.0 as i16, -350);
            assert_eq!((message.lParam.0 >> 16) as i16, 175);

            input.set_popup(None).unwrap();
            let _ = send_wheel();
            assert!(!receive_wheel(&mut message));
            drop(input);
            DestroyWindow(hwnd).unwrap();
        }
    }

    fn tap(taps: &mut CtrlTaps, side: u8, time: u32) -> bool {
        assert!(!taps.ctrl(side, true, time, false));
        taps.ctrl(side, false, time.wrapping_add(35), false)
    }

    #[test]
    fn transparent_popup_corners_count_as_outside_clicks() {
        let bounds = PopupInput {
            rect: RectData {
                left: -310,
                top: 100,
                right: -1,
                bottom: 358,
            },
            corner_radius: 10,
            has_scenes: true,
        };
        assert!(!bounds.contains(POINT { x: -310, y: 100 }));
        assert!(!bounds.contains(POINT { x: -2, y: 357 }));
        assert!(bounds.contains(POINT { x: -295, y: 100 }));
        assert!(bounds.contains(POINT { x: -310, y: 150 }));
        assert!(PopupInput {
            corner_radius: 0,
            ..bounds
        }
        .contains(POINT { x: -310, y: 100 }));
    }

    #[test]
    fn two_released_ctrl_taps_trigger_once_even_across_tick_wrap() {
        let mut taps = CtrlTaps::default();
        assert!(!tap(&mut taps, LEFT_CTRL, u32::MAX - 50));
        assert!(tap(&mut taps, RIGHT_CTRL, 30));
        assert!(!tap(&mut taps, RIGHT_CTRL, 130));
    }

    #[test]
    fn slow_taps_and_key_repeat_do_not_trigger() {
        let mut taps = CtrlTaps::default();
        assert!(!tap(&mut taps, LEFT_CTRL, 0));
        assert!(!taps.ctrl(LEFT_CTRL, true, 100, false));
        assert!(!taps.ctrl(LEFT_CTRL, true, 400, false));
        assert!(!taps.ctrl(LEFT_CTRL, false, 600, false));
        assert!(!tap(&mut taps, LEFT_CTRL, 650));
        assert!(!tap(&mut taps, LEFT_CTRL, 1200));
    }

    #[test]
    fn chords_and_overlapping_ctrls_cancel_the_sequence() {
        let mut taps = CtrlTaps::default();
        assert!(!tap(&mut taps, LEFT_CTRL, 0));
        assert!(!taps.ctrl(LEFT_CTRL, true, 100, false));
        taps.interrupt(); // Ctrl+C or Ctrl+click.
        assert!(!taps.ctrl(LEFT_CTRL, false, 135, false));
        assert!(!tap(&mut taps, LEFT_CTRL, 200));
        assert!(!taps.ctrl(LEFT_CTRL, true, 260, false));
        assert!(!taps.ctrl(RIGHT_CTRL, true, 270, false));
        assert!(!taps.ctrl(LEFT_CTRL, false, 285, false));
        assert!(!taps.ctrl(RIGHT_CTRL, false, 300, false));
        assert!(!taps.ctrl(RIGHT_CTRL, true, 310, true));
        assert!(!taps.ctrl(RIGHT_CTRL, false, 340, false));
        assert!(!tap(&mut taps, RIGHT_CTRL, 400));
        assert!(tap(&mut taps, RIGHT_CTRL, 480));
    }
}
