//! Non-activating launcher shown next to the cursor.

mod paint;

use self::paint::{PopupFonts, PopupPalette};
use super::input::point_in_rect;
use super::paint::{GdiThemeCache, GdiplusContext};
use crate::config::{AppConfig, RectData, Scene, Shortcut};
use crate::platform::cursor_input::{PopupBounds, WM_APP_DISMISS_CURSOR_POPUP};
use crate::platform::monitors::{clamp_and_snap_in_work_area, dip_to_px};
use crate::platform::MonitorInfoData;
use std::mem::size_of;
use std::sync::Once;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, InvalidateRect, SetWindowRgn,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_CURSOR_POPUP_ACTION: u32 = 0x8000 + 9;
const WM_MOUSELEAVE: u32 = 0x02A3;
const SCENE_COLUMNS: usize = 3;
const WIDTH_DIP: i32 = 309;
const CORNER_RADIUS_DIP: i32 = 10;

fn popup_bounds(cursor: POINT, width: i32, height: i32, monitor: &MonitorInfoData) -> RectData {
    let work = monitor.work_area;
    let gap = dip_to_px(8, monitor.dpi);
    let x = if cursor.x + gap + width <= work.right {
        cursor.x + gap
    } else {
        cursor.x - gap - width
    };
    let y = if cursor.y + gap + height <= work.bottom {
        cursor.y + gap
    } else {
        cursor.y - gap - height
    };
    let (x, y) = clamp_and_snap_in_work_area(x, y, width, height, work, monitor.dpi, false);
    RectData {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

struct PopupLayout {
    pad: i32,
    gap: i32,
    scene_top: i32,
    tile_height: i32,
    row_height: i32,
    divider_top: Option<i32>,
    shortcut_header_top: i32,
    shortcut_top: i32,
    content_height: i32,
}

impl PopupLayout {
    fn new(dpi: u32, scenes: usize, shortcuts: usize) -> Self {
        let px = |dip| dip_to_px(dip, dpi);
        let pad = px(13);
        let gap = px(5);
        let scene_top = pad + px(24);
        let tile_height = px(58);
        let row_height = px(36);
        let scene_rows = scenes.div_ceil(SCENE_COLUMNS) as i32;
        let divider_top =
            (scene_rows > 0).then(|| scene_top + (tile_height + gap) * scene_rows - gap + px(11));
        let shortcut_header_top = divider_top.map_or(pad, |top| top + px(11));
        let shortcut_top = shortcut_header_top + px(20);
        let content_height = shortcut_top + shortcuts.max(1) as i32 * row_height + pad;
        Self {
            pad,
            gap,
            scene_top,
            tile_height,
            row_height,
            divider_top,
            shortcut_header_top,
            shortcut_top,
            content_height,
        }
    }

    fn scene(&self, index: usize, width: i32) -> RectData {
        let col = (index % SCENE_COLUMNS) as i32;
        let row = (index / SCENE_COLUMNS) as i32;
        let columns = SCENE_COLUMNS as i32;
        let span = (width - self.pad * 2 - self.gap * (columns - 1)).max(0);
        let top = self.scene_top + row * (self.tile_height + self.gap);
        RectData {
            left: self.pad + self.gap * col + span * col / columns,
            right: self.pad + self.gap * col + span * (col + 1) / columns,
            top,
            bottom: top + self.tile_height,
        }
    }

    fn shortcut(&self, index: usize, width: i32) -> RectData {
        let top = self.shortcut_top + index as i32 * self.row_height;
        RectData {
            left: self.pad,
            right: width - self.pad,
            top,
            bottom: top + self.row_height,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Scene(usize),
    Shortcut(usize),
    Settings,
}

pub enum PopupAction {
    SwitchScene(String),
    OpenShortcut(Shortcut),
    OpenSettings,
}

pub struct CursorPopup {
    hwnd: HWND,
    owner: HWND,
    pub visible: bool,
    scenes: Vec<Scene>,
    shortcuts: Vec<Shortcut>,
    active_scene_id: Option<String>,
    dpi: u32,
    layout: PopupLayout,
    window_width: i32,
    width: i32,
    height: i32,
    scroll: i32,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking_leave: bool,
    pub pending_action: Option<PopupAction>,
    icon_cache: GdiThemeCache,
    fonts: PopupFonts,
    palette: PopupPalette,
}

impl CursorPopup {
    pub fn create(owner: HWND, gdiplus: GdiplusContext) -> windows::core::Result<Box<Self>> {
        static REGISTER: Once = Once::new();
        let class = w!("KiriCursorPopupClass");
        unsafe {
            let module = GetModuleHandleW(None)?;
            REGISTER.call_once(|| {
                let _ = RegisterClassW(&WNDCLASSW {
                    style: CS_DROPSHADOW,
                    lpfnWndProc: Some(window_proc),
                    hInstance: module.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class,
                    ..Default::default()
                });
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Kiri ランチャー"),
                WS_POPUP | WS_VSCROLL,
                0,
                0,
                0,
                0,
                None,
                None,
                module,
                None,
            )?;
            let mut popup = Box::new(Self {
                hwnd,
                owner,
                visible: false,
                scenes: Vec::new(),
                shortcuts: Vec::new(),
                active_scene_id: None,
                dpi: 96,
                layout: PopupLayout::new(96, 0, 0),
                window_width: 0,
                width: 0,
                height: 0,
                scroll: 0,
                hover: None,
                pressed: None,
                tracking_leave: false,
                pending_action: None,
                icon_cache: GdiThemeCache::with_context(96, gdiplus),
                fonts: PopupFonts::new(96),
                palette: PopupPalette::system(),
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *popup as *mut Self) as isize);
            Ok(popup)
        }
    }

    pub fn show(
        &mut self,
        cursor: POINT,
        monitor: &MonitorInfoData,
        config: &AppConfig,
        active_scene_id: Option<&str>,
    ) -> windows::core::Result<PopupBounds> {
        self.scenes = config.ordered_scenes().into_iter().cloned().collect();
        self.shortcuts.clone_from(&config.shortcuts);
        self.active_scene_id = active_scene_id.map(str::to_owned);
        self.layout = PopupLayout::new(monitor.dpi, self.scenes.len(), self.shortcuts.len());
        self.palette = PopupPalette::system();
        if self.dpi != monitor.dpi {
            self.dpi = monitor.dpi;
            self.icon_cache.ensure_dpi(self.dpi);
            self.fonts = PopupFonts::new(self.dpi);
        }

        let width = dip_to_px(WIDTH_DIP, self.dpi).min(monitor.work_area.width().max(1));
        let height = self
            .layout
            .content_height
            .min(dip_to_px(430, self.dpi))
            .min(monitor.work_area.height().max(1));
        let bounds = popup_bounds(cursor, width, height, monitor);
        self.scroll = 0;
        self.hover = None;
        self.pressed = None;
        self.tracking_leave = false;
        self.pending_action = None;
        self.window_width = width;

        let mut corner_radius = 0;
        unsafe {
            SetScrollInfo(
                self.hwnd,
                SB_VERT,
                &SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                    nMin: 0,
                    nMax: self.layout.content_height - 1,
                    nPage: height as u32,
                    nPos: 0,
                    ..Default::default()
                },
                false,
            );
            let radius = dip_to_px(CORNER_RADIUS_DIP, self.dpi)
                .min(width / 2)
                .min(height / 2);
            let diameter = radius * 2;
            let region = CreateRoundRectRgn(0, 0, width, height, diameter, diameter);
            if !region.is_invalid() {
                if SetWindowRgn(self.hwnd, region, false) != 0 {
                    corner_radius = radius;
                } else {
                    let _ = DeleteObject(region);
                }
            }
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                bounds.left,
                bounds.top,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            self.visible = true;
            let mut client = RECT::default();
            GetClientRect(self.hwnd, &mut client)?;
            self.width = client.right;
            self.height = client.bottom;
            let _ = InvalidateRect(self.hwnd, None, false);
        }
        Ok(PopupBounds {
            rect: bounds,
            corner_radius,
        })
    }

    pub fn hide(&mut self) {
        self.pending_action = None;
        if self.visible {
            self.visible = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    fn hit_test(&self, lparam: LPARAM) -> Option<Hit> {
        let x = lparam.0 as i16 as i32;
        let y = ((lparam.0 >> 16) as i16 as i32) + self.scroll;
        if let Some(index) =
            (0..self.scenes.len()).find(|&i| point_in_rect(x, y, &self.layout.scene(i, self.width)))
        {
            return Some(Hit::Scene(index));
        }
        let index = (0..self.shortcuts.len().max(1))
            .find(|&i| point_in_rect(x, y, &self.layout.shortcut(i, self.width)))?;
        Some(if self.shortcuts.is_empty() {
            Hit::Settings
        } else {
            Hit::Shortcut(index)
        })
    }

    unsafe fn scroll_to(&mut self, requested: i32) {
        let scroll = requested.clamp(0, (self.layout.content_height - self.height).max(0));
        if scroll != self.scroll {
            self.scroll = scroll;
            self.hover = None;
            self.pressed = None;
            SetScrollInfo(
                self.hwnd,
                SB_VERT,
                &SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_POS,
                    nPos: scroll,
                    ..Default::default()
                },
                true,
            );
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn viewport_rect(&self, mut rect: RectData) -> Option<RectData> {
        rect.top -= self.scroll;
        rect.bottom -= self.scroll;
        (rect.bottom > 0 && rect.top < self.height).then_some(rect)
    }
}

impl Drop for CursorPopup {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn win_rect(rect: RectData) -> RECT {
    RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let result = std::panic::catch_unwind(|| {
        // SetWindowPos and scrollbar updates can synchronously send messages. Only borrow the
        // popup for messages that use its state.
        match msg {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => return LRESULT(0),
            WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP
            | WM_MOUSEWHEEL | WM_VSCROLL | WM_PAINT | WM_SETTINGCHANGE => {}
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CursorPopup;
        let Some(popup) = ptr.as_mut() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_SETTINGCHANGE => {
                if popup.visible {
                    let palette = PopupPalette::system();
                    if popup.palette != palette {
                        popup.palette = palette;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
            }
            WM_MOUSEMOVE => {
                if !popup.tracking_leave {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    popup.tracking_leave = TrackMouseEvent(&mut track).is_ok();
                }
                let hit = popup.hit_test(lparam);
                if hit != popup.hover {
                    popup.hover = hit;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            WM_MOUSELEAVE => {
                popup.tracking_leave = false;
                popup.pressed = None;
                if popup.hover.take().is_some() {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            WM_LBUTTONDOWN => popup.pressed = popup.hit_test(lparam),
            WM_LBUTTONUP => {
                let Some(hit) = popup.pressed.take() else {
                    return LRESULT(0);
                };
                if popup.visible
                    && popup.hit_test(lparam) == Some(hit)
                    && popup.pending_action.is_none()
                {
                    popup.pending_action = Some(match hit {
                        Hit::Scene(index) => {
                            PopupAction::SwitchScene(popup.scenes[index].id.clone())
                        }
                        Hit::Shortcut(index) => {
                            PopupAction::OpenShortcut(popup.shortcuts[index].clone())
                        }
                        Hit::Settings => PopupAction::OpenSettings,
                    });
                    if PostMessageW(
                        popup.owner,
                        WM_APP_CURSOR_POPUP_ACTION,
                        WPARAM(0),
                        LPARAM(0),
                    )
                    .is_err()
                    {
                        popup.pending_action = None;
                    }
                }
            }
            WM_RBUTTONUP => {
                let _ = PostMessageW(
                    popup.owner,
                    WM_APP_DISMISS_CURSOR_POPUP,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) as i16) as i32;
                popup.scroll_to(popup.scroll - delta * popup.layout.row_height / 120);
            }
            WM_VSCROLL => {
                let next = match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
                    SB_LINEUP => popup.scroll - popup.layout.row_height,
                    SB_LINEDOWN => popup.scroll + popup.layout.row_height,
                    SB_PAGEUP => popup.scroll - popup.height,
                    SB_PAGEDOWN => popup.scroll + popup.height,
                    SB_TOP => 0,
                    SB_BOTTOM => popup.layout.content_height,
                    SB_THUMBTRACK | SB_THUMBPOSITION => {
                        let mut info = SCROLLINFO {
                            cbSize: size_of::<SCROLLINFO>() as u32,
                            fMask: SIF_TRACKPOS,
                            ..Default::default()
                        };
                        if GetScrollInfo(hwnd, SB_VERT, &mut info).is_err() {
                            return LRESULT(0);
                        }
                        info.nTrackPos
                    }
                    _ => return LRESULT(0),
                };
                popup.scroll_to(next);
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                popup.paint(hdc);
                let _ = EndPaint(hwnd, &ps);
            }
            _ => {}
        }
        LRESULT(0)
    });
    result.unwrap_or_else(|_| DefWindowProcW(hwnd, msg, wparam, lparam))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShortcutKind;

    #[test]
    fn popup_opens_inward_on_a_high_dpi_monitor_left_of_primary() {
        let monitor = MonitorInfoData {
            monitor_id: String::new(),
            friendly_name: String::new(),
            monitor_rect: RectData {
                left: -1920,
                top: -100,
                right: 0,
                bottom: 980,
            },
            work_area: RectData {
                left: -1920,
                top: -100,
                right: 0,
                bottom: 930,
            },
            dpi: 144,
            is_primary: false,
        };
        let bounds = popup_bounds(POINT { x: -3, y: 965 }, 462, 400, &monitor);
        assert_eq!(
            bounds,
            RectData {
                left: -477,
                top: 530,
                right: -15,
                bottom: 930
            }
        );
        let bounds = popup_bounds(POINT { x: -1910, y: -90 }, 462, 400, &monitor);
        assert_eq!(
            bounds,
            RectData {
                left: -1898,
                top: -78,
                right: -1436,
                bottom: 322
            }
        );
    }

    #[test]
    fn native_popup_keeps_focus_and_routes_click_after_scrolling() {
        unsafe {
            let work_area = RectData {
                left: -20000,
                top: -20000,
                right: -18976,
                bottom: -19232,
            };
            let monitor = MonitorInfoData {
                monitor_id: String::new(),
                friendly_name: String::new(),
                monitor_rect: work_area,
                work_area,
                dpi: 96,
                is_primary: false,
            };
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
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
            let mut config = AppConfig::default_initial();
            config.shortcuts = (0..14)
                .map(|i| Shortcut {
                    name: format!("Site {i}"),
                    kind: ShortcutKind::Website,
                    target: format!("https://example.com/{i}"),
                })
                .collect();
            let mut popup = CursorPopup::create(owner, GdiplusContext::init()).unwrap();
            let cursor = POINT {
                x: work_area.left + 40,
                y: work_area.top + 40,
            };
            let bounds = popup.show(cursor, &monitor, &config, Some("code")).unwrap();
            let mut actual = RECT::default();
            GetWindowRect(popup.hwnd, &mut actual).unwrap();
            assert_eq!(actual, win_rect(bounds.rect));
            assert_eq!(bounds.corner_radius, CORNER_RADIUS_DIP);
            assert!(IsWindowVisible(popup.hwnd).as_bool());
            assert_ne!(GetForegroundWindow(), popup.hwnd);
            assert_eq!(
                SendMessageW(popup.hwnd, WM_MOUSEACTIVATE, WPARAM(0), LPARAM(0)).0,
                MA_NOACTIVATE as isize
            );

            let _ = SendMessageW(
                popup.hwnd,
                WM_VSCROLL,
                WPARAM(SB_BOTTOM.0 as usize),
                LPARAM(0),
            );
            assert!(popup.scroll > 0);
            let last = config.shortcuts.len() - 1;
            let row = popup
                .viewport_rect(popup.layout.shortcut(last, popup.width))
                .unwrap();
            let hit = LPARAM((((row.top + 3) as isize) << 16) | (row.left + 3) as isize);
            let _ = SendMessageW(popup.hwnd, WM_LBUTTONDOWN, WPARAM(0), hit);
            let _ = SendMessageW(popup.hwnd, WM_LBUTTONUP, WPARAM(0), hit);
            assert!(
                matches!(popup.pending_action.take(), Some(PopupAction::OpenShortcut(shortcut))
                if shortcut == config.shortcuts[last])
            );

            let mut message = MSG::default();
            assert!(PeekMessageW(
                &mut message,
                owner,
                WM_APP_CURSOR_POPUP_ACTION,
                WM_APP_CURSOR_POPUP_ACTION,
                PM_REMOVE
            )
            .as_bool());
            popup.hide();
            assert!(!IsWindowVisible(popup.hwnd).as_bool());
            drop(popup);
            DestroyWindow(owner).unwrap();
        }
    }
}
