//! Observe Ctrl taps and outside clicks, and route the wheel to the non-activating launcher.

use crate::config::RectData;
use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LWIN,
    VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE,
    VK_UP, VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WindowFromPoint, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_XBUTTONDOWN,
};

pub const WM_APP_SHOW_CURSOR_POPUP: u32 = 0x8000 + 7;
pub const WM_APP_DISMISS_CURSOR_POPUP: u32 = 0x8000 + 8;
pub const WM_APP_CURSOR_POPUP_WHEEL: u32 = 0x8000 + 11;
pub const WM_APP_CURSOR_POPUP_KEY: u32 = 0x8000 + 12;
pub const WM_APP_CANCEL_VARIANT_FAN: u32 = 0x8000 + 13;
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

#[derive(Debug, PartialEq, Eq)]
enum CtrlTap {
    Single,
    Double,
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

    fn ctrl(&mut self, side: u8, down: bool, time: u32, chord_held: bool) -> Option<CtrlTap> {
        if down {
            if self.held & side == 0 {
                if self.held != 0 || chord_held {
                    self.interrupt();
                } else {
                    self.pressed_at = Some(time);
                }
                self.held |= side;
            }
            return None;
        }

        if self.held & side == 0 {
            return None;
        }
        self.held &= !side;
        let press = self.pressed_at.take()?;
        if time.wrapping_sub(press) > DOUBLE_TAP_MS {
            self.interrupt();
            return None;
        }
        if self
            .first_release
            .take()
            .is_some_and(|first| time.wrapping_sub(first) <= DOUBLE_TAP_MS)
        {
            return Some(CtrlTap::Double);
        }
        self.first_release = Some(time);
        Some(CtrlTap::Single)
    }
}

#[derive(Clone, Copy, Default)]
struct InputState {
    hwnd: HWND,
    ctrl_enabled: bool,
    taps: CtrlTaps,
    popup: Option<PopupInput>,
    popup_fan_active: bool,
    bar_hovered: bool,
    bar_fan_active: bool,
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

    fn update_keyboard_hook(&mut self, state: InputState) -> windows::core::Result<()> {
        let needed = state.ctrl_enabled || state.popup.is_some() || state.bar_fan_active;
        if needed && self.keyboard_hook.is_none() {
            self.keyboard_hook = Some(unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_proc),
                    GetModuleHandleW(None)?,
                    0,
                )?
            });
        } else if !needed {
            if let Some(hook) = self.keyboard_hook.take() {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
        }
        Ok(())
    }

    pub fn set_ctrl_enabled(&mut self, enabled: bool) -> windows::core::Result<()> {
        let mut state = STATE.get();
        state.ctrl_enabled = enabled;
        if !enabled {
            state.taps = CtrlTaps::default();
        }
        STATE.set(state);
        self.update_keyboard_hook(state)?;
        update_mouse_hook(state)
    }

    pub fn interrupt_gesture(&self) {
        let mut state = STATE.get();
        state.taps.interrupt();
        STATE.set(state);
    }

    pub fn reset_gesture(&self) {
        let mut state = STATE.get();
        state.taps = CtrlTaps::default();
        STATE.set(state);
        let _ = update_mouse_hook(state);
    }

    pub fn set_popup(&mut self, popup: Option<PopupInput>) -> windows::core::Result<()> {
        let mut state = STATE.get();
        state.popup = popup;
        if popup.is_none() {
            state.popup_fan_active = false;
        }
        STATE.set(state);
        self.update_keyboard_hook(state)?;
        update_mouse_hook(state)
    }

    pub fn set_popup_fan_active(&self, active: bool) {
        let mut state = STATE.get();
        state.popup_fan_active = active;
        STATE.set(state);
    }

    pub fn set_bar_fan_active(&mut self, active: bool) -> windows::core::Result<()> {
        let mut state = STATE.get();
        state.bar_fan_active = active;
        STATE.set(state);
        self.update_keyboard_hook(state)
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
        let state = InputState::default();
        STATE.set(state);
        let _ = self.update_keyboard_hook(state);
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
    let consumed = std::panic::catch_unwind(|| {
        if code != HC_ACTION as i32 {
            return false;
        }
        let key = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let mut state = STATE.get();
        let down = !key.flags.contains(LLKHF_UP);
        let vk = key.vkCode as u16;

        // While the non-activating cursor popup is open, route its navigation keys to it.
        if state.popup.is_some()
            && matches!(
                vk,
                v if v == VK_ESCAPE.0
                    || v == VK_RETURN.0
                    || v == VK_SPACE.0
                    || v == VK_UP.0
                    || v == VK_DOWN.0
                    || v == VK_LEFT.0
                    || v == VK_RIGHT.0
                    || (state.popup_fan_active && (b'1'..=b'9').contains(&(v as u8)))
            )
        {
            state.taps.interrupt();
            STATE.set(state);
            if down {
                let _ = PostMessageW(
                    state.hwnd,
                    WM_APP_CURSOR_POPUP_KEY,
                    WPARAM(vk as usize),
                    LPARAM(0),
                );
            }
            return true;
        }

        if state.bar_fan_active && vk == VK_ESCAPE.0 {
            if down {
                let _ = PostMessageW(state.hwnd, WM_APP_CANCEL_VARIANT_FAN, WPARAM(0), LPARAM(0));
            }
            return true;
        }

        if !state.ctrl_enabled {
            return false;
        }
        let side = match vk {
            code if code == VK_LCONTROL.0 => LEFT_CTRL,
            code if code == VK_RCONTROL.0 => RIGHT_CTRL,
            code if code == VK_CONTROL.0 && key.flags.contains(LLKHF_EXTENDED) => RIGHT_CTRL,
            code if code == VK_CONTROL.0 => LEFT_CTRL,
            _ => {
                state.taps.interrupt();
                STATE.set(state);
                return false;
            }
        };
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
        let tap = state.taps.ctrl(side, down, key.time, chord_held);
        STATE.set(state);
        match tap {
            Some(CtrlTap::Single) if state.popup.is_some() => {
                // Keep the first release available to pair with the next tap after hiding.
                let _ = PostMessageW(
                    state.hwnd,
                    WM_APP_DISMISS_CURSOR_POPUP,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            Some(CtrlTap::Double) => {
                let mut cursor = POINT::default();
                if GetCursorPos(&mut cursor).is_ok() {
                    let _ = PostMessageW(
                        state.hwnd,
                        WM_APP_SHOW_CURSOR_POPUP,
                        WPARAM(cursor.x as usize),
                        LPARAM(cursor.y as isize),
                    );
                }
            }
            _ => {}
        }
        if (was_held == 0) != (state.taps.held == 0) {
            let _ = update_mouse_hook(state);
        }
        false
    })
    .unwrap_or(false);
    if consumed {
        return LRESULT(1);
    }
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
            if button
                && !state.popup_fan_active
                && state.popup.is_some_and(|popup| !popup.contains(mouse.pt))
            {
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
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_REMOVE,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_KEYUP,
    };

    fn input_window() -> (HWND, CursorInput) {
        let hwnd = unsafe {
            CreateWindowExW(
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
            .unwrap()
        };
        (hwnd, CursorInput::new(hwnd))
    }

    #[test]
    fn wheel_at_the_invocation_point_routes_only_while_the_scene_popup_is_open() {
        unsafe {
            let (hwnd, mut input) = input_window();
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

    fn tap(taps: &mut CtrlTaps, side: u8, time: u32) -> Option<CtrlTap> {
        assert_eq!(taps.ctrl(side, true, time, false), None);
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
        assert_eq!(
            tap(&mut taps, LEFT_CTRL, u32::MAX - 50),
            Some(CtrlTap::Single)
        );
        assert_eq!(tap(&mut taps, RIGHT_CTRL, 30), Some(CtrlTap::Double));
        assert_eq!(tap(&mut taps, RIGHT_CTRL, 130), Some(CtrlTap::Single));
    }

    #[test]
    fn long_holds_and_key_repeat_do_not_count_as_taps() {
        let mut taps = CtrlTaps::default();
        assert_eq!(tap(&mut taps, LEFT_CTRL, 0), Some(CtrlTap::Single));
        assert_eq!(taps.ctrl(LEFT_CTRL, true, 100, false), None);
        assert_eq!(taps.ctrl(LEFT_CTRL, true, 400, false), None);
        assert_eq!(taps.ctrl(LEFT_CTRL, false, 600, false), None);
        assert_eq!(tap(&mut taps, LEFT_CTRL, 650), Some(CtrlTap::Single));
        assert_eq!(
            tap(&mut taps, LEFT_CTRL, 650 + DOUBLE_TAP_MS + 1),
            Some(CtrlTap::Single)
        );
    }

    #[test]
    fn chords_and_overlapping_ctrls_cancel_the_sequence() {
        let mut taps = CtrlTaps::default();
        assert_eq!(tap(&mut taps, LEFT_CTRL, 0), Some(CtrlTap::Single));
        assert_eq!(taps.ctrl(LEFT_CTRL, true, 100, false), None);
        taps.interrupt(); // Ctrl+C or Ctrl+click.
        assert_eq!(taps.ctrl(LEFT_CTRL, false, 135, false), None);
        assert_eq!(tap(&mut taps, LEFT_CTRL, 200), Some(CtrlTap::Single));
        assert_eq!(taps.ctrl(LEFT_CTRL, true, 260, false), None);
        assert_eq!(taps.ctrl(RIGHT_CTRL, true, 270, false), None);
        assert_eq!(taps.ctrl(LEFT_CTRL, false, 285, false), None);
        assert_eq!(taps.ctrl(RIGHT_CTRL, false, 300, false), None);
        assert_eq!(taps.ctrl(RIGHT_CTRL, true, 310, true), None);
        assert_eq!(taps.ctrl(RIGHT_CTRL, false, 340, false), None);
        assert_eq!(tap(&mut taps, RIGHT_CTRL, 400), Some(CtrlTap::Single));
        assert_eq!(tap(&mut taps, RIGHT_CTRL, 480), Some(CtrlTap::Double));
    }

    #[test]
    fn ctrl_taps_close_and_reopen_the_cursor_popup() {
        unsafe {
            let (hwnd, mut input) = input_window();
            input.set_ctrl_enabled(true).unwrap();
            let send_tap = |time| {
                // Prepare the keydown without consulting the desktop's live modifier state.
                let mut state = STATE.get();
                assert_eq!(state.taps.ctrl(LEFT_CTRL, true, time, false), None);
                STATE.set(state);
                let release = KBDLLHOOKSTRUCT {
                    vkCode: u32::from(VK_LCONTROL.0),
                    flags: LLKHF_UP,
                    time: time + 35,
                    ..Default::default()
                };
                let result = keyboard_proc(
                    HC_ACTION as i32,
                    WPARAM(WM_KEYUP as usize),
                    LPARAM(&release as *const _ as isize),
                );
                assert_eq!(result.0, 0);
            };
            let take_request = || {
                let mut message = MSG::default();
                PeekMessageW(
                    &mut message,
                    hwnd,
                    WM_APP_SHOW_CURSOR_POPUP,
                    WM_APP_DISMISS_CURSOR_POPUP,
                    PM_REMOVE,
                )
                .as_bool()
                .then_some(message.message)
            };
            let popup = PopupInput {
                rect: RectData::default(),
                corner_radius: 0,
                has_scenes: true,
            };

            send_tap(0);
            assert_eq!(take_request(), None);
            send_tap(150);
            assert_eq!(take_request(), Some(WM_APP_SHOW_CURSOR_POPUP));

            input.set_popup(Some(popup)).unwrap();
            send_tap(300);
            assert_eq!(take_request(), Some(WM_APP_DISMISS_CURSOR_POPUP));
            assert_eq!(take_request(), None);
            input.set_popup(None).unwrap();
            send_tap(300 + DOUBLE_TAP_MS);
            assert_eq!(take_request(), Some(WM_APP_SHOW_CURSOR_POPUP));

            // The second tap still means show when the UI hasn't processed the hide request.
            input.set_popup(Some(popup)).unwrap();
            send_tap(900);
            send_tap(1050);
            assert_eq!(take_request(), Some(WM_APP_DISMISS_CURSOR_POPUP));
            assert_eq!(take_request(), Some(WM_APP_SHOW_CURSOR_POPUP));
            assert_eq!(take_request(), None);
            drop(input);
            DestroyWindow(hwnd).unwrap();
        }
    }

    #[test]
    fn popup_navigation_keys_are_intercepted_only_when_popup_is_open() {
        unsafe {
            let (hwnd, mut input) = input_window();
            let kb = KBDLLHOOKSTRUCT {
                vkCode: u32::from(VK_ESCAPE.0),
                ..Default::default()
            };
            let send_esc = || {
                keyboard_proc(
                    HC_ACTION as i32,
                    WPARAM(0x0100),
                    LPARAM(&kb as *const _ as isize),
                )
            };

            // Closed -> not intercepted
            assert_eq!(send_esc().0, 0);

            let popup = PopupInput {
                rect: RectData {
                    left: 0,
                    top: 0,
                    right: 200,
                    bottom: 200,
                },
                corner_radius: 10,
                has_scenes: true,
            };
            input.set_popup(Some(popup)).unwrap();
            assert_eq!(send_esc().0, 1);

            let mut msg = MSG::default();
            assert!(PeekMessageW(
                &mut msg,
                hwnd,
                WM_APP_CURSOR_POPUP_KEY,
                WM_APP_CURSOR_POPUP_KEY,
                PM_REMOVE,
            )
            .as_bool());
            assert_eq!(msg.wParam.0, usize::from(VK_ESCAPE.0));

            input.set_popup(None).unwrap();
            assert_eq!(send_esc().0, 0);
            drop(input);
            DestroyWindow(hwnd).unwrap();
        }
    }

    #[test]
    fn fan_hooks_suppress_outside_dismiss_and_route_cancel_keys() {
        unsafe {
            let (hwnd, mut input) = input_window();

            // Bar fan active intercepts Escape without taking focus
            input.set_bar_fan_active(true).unwrap();
            let kb_esc = KBDLLHOOKSTRUCT {
                vkCode: u32::from(VK_ESCAPE.0),
                ..Default::default()
            };
            assert_eq!(
                keyboard_proc(
                    HC_ACTION as i32,
                    WPARAM(0x0100),
                    LPARAM(&kb_esc as *const _ as isize),
                )
                .0,
                1
            );
            let mut msg = MSG::default();
            assert!(PeekMessageW(
                &mut msg,
                hwnd,
                WM_APP_CANCEL_VARIANT_FAN,
                WM_APP_CANCEL_VARIANT_FAN,
                PM_REMOVE,
            )
            .as_bool());
            input.set_bar_fan_active(false).unwrap();

            // Cursor popup fan active keeps outside clicks from prematurely dismissing the popup
            let popup = PopupInput {
                rect: RectData {
                    left: 0,
                    top: 0,
                    right: 200,
                    bottom: 200,
                },
                corner_radius: 10,
                has_scenes: true,
            };
            input.set_popup(Some(popup)).unwrap();
            input.set_popup_fan_active(true);

            let outside = MSLLHOOKSTRUCT {
                pt: POINT { x: 250, y: 50 },
                ..Default::default()
            };
            let _ = mouse_proc(
                HC_ACTION as i32,
                WPARAM(WM_LBUTTONDOWN as usize),
                LPARAM(&outside as *const _ as isize),
            );
            assert!(!PeekMessageW(
                &mut msg,
                hwnd,
                WM_APP_DISMISS_CURSOR_POPUP,
                WM_APP_DISMISS_CURSOR_POPUP,
                PM_REMOVE,
            )
            .as_bool());

            // Number keys route to popup while fan is active
            let kb_two = KBDLLHOOKSTRUCT {
                vkCode: usize::from(b'2') as u32,
                ..Default::default()
            };
            assert_eq!(
                keyboard_proc(
                    HC_ACTION as i32,
                    WPARAM(0x0100),
                    LPARAM(&kb_two as *const _ as isize),
                )
                .0,
                1
            );
            assert!(PeekMessageW(
                &mut msg,
                hwnd,
                WM_APP_CURSOR_POPUP_KEY,
                WM_APP_CURSOR_POPUP_KEY,
                PM_REMOVE,
            )
            .as_bool());

            input.set_popup(None).unwrap();
            drop(input);
            DestroyWindow(hwnd).unwrap();
        }
    }
}
