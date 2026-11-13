//! Non-activating launcher shown next to the cursor.

mod paint;

use self::paint::{PopupFonts, PopupPalette};
use super::input::{point_in_rect, SceneWheel};
use super::paint::{GdiThemeCache, GdiplusContext};
use crate::config::{AppConfig, CursorPopupPosition, RectData, Scene, Shortcut};
use crate::platform::cursor_input::{PopupInput, WM_APP_CURSOR_POPUP_WHEEL, WM_APP_DISMISS_CURSOR_POPUP};
use crate::platform::monitors::{clamp_and_snap_in_work_area, dip_to_px};
use crate::platform::MonitorInfoData;
use std::mem::size_of;
use std::sync::Once;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, InvalidateRect, ScreenToClient,
    SetWindowRgn, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_CURSOR_POPUP_ACTION: u32 = 0x8000 + 9;
const WM_MOUSELEAVE: u32 = 0x02A3;
const SCENE_COLUMNS: usize = 3;
const WIDTH_DIP: i32 = 309;
const CURSOR_GAP_DIP: i32 = 8;
const CORNER_RADIUS_DIP: i32 = 10;

fn bounds_beside_cursor(cursor: POINT, width: i32, height: i32, monitor: &MonitorInfoData) -> RectData {
    let work = monitor.work_area;
    let gap = dip_to_px(CURSOR_GAP_DIP, monitor.dpi);
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
    scene_header_top: i32,
    scene_top: i32,
    tile_height: i32,
    row_height: i32,
    divider_top: Option<i32>,
    shortcut_header_top: i32,
    shortcut_top: i32,
    content_height: i32,
}

impl PopupLayout {
    fn new(dpi: u32, scenes: usize, shortcuts: usize, scenes_at_bottom: bool) -> Self {
        let px = |dip| dip_to_px(dip, dpi);
        let pad = px(13);
        let gap = px(5);
        let tile_height = px(58);
        let row_height = px(36);
        let scene_header_height = px(24);
        let shortcut_header_height = px(20);
        let section_gap = px(11);
        let scene_rows = scenes.div_ceil(SCENE_COLUMNS) as i32;
        let scene_height = if scene_rows > 0 {
            scene_header_height + (tile_height + gap) * scene_rows - gap
        } else {
            0
        };
        let shortcut_height = shortcut_header_height + shortcuts.max(1) as i32 * row_height;
        let (scene_header_top, shortcut_header_top, divider_top) = if scenes == 0 {
            (pad, pad, None)
        } else if scenes_at_bottom {
            let divider = pad + shortcut_height + section_gap;
            (divider + section_gap, pad, Some(divider))
        } else {
            let divider = pad + scene_height + section_gap;
            (pad, divider + section_gap, Some(divider))
        };
        let content_height =
            (scene_header_top + scene_height).max(shortcut_header_top + shortcut_height) + pad;
        Self {
            pad,
            gap,
            scene_header_top,
            scene_top: scene_header_top + scene_header_height,
            tile_height,
            row_height,
            divider_top,
            shortcut_header_top,
            shortcut_top: shortcut_header_top + shortcut_header_height,
            content_height,
        }
    }

    fn place(
        cursor: POINT,
        monitor: &MonitorInfoData,
        scenes: usize,
        shortcuts: usize,
        position: CursorPopupPosition,
    ) -> (Self, RectData, i32) {
        let dpi = monitor.dpi;
        let work = monitor.work_area;
        let width = dip_to_px(WIDTH_DIP, dpi).min(work.width().max(1));
        let mut layout = Self::new(dpi, scenes, shortcuts, false);
        let max_height = layout
            .content_height
            .min(dip_to_px(430, dpi))
            .min(work.height().max(1));
        if position == CursorPopupPosition::BesideCursor || scenes == 0 {
            let bounds = bounds_beside_cursor(cursor, width, max_height, monitor);
            return (layout, bounds, 0);
        }

        let first_row_right = layout.scene(scenes.min(SCENE_COLUMNS) - 1, width).right;
        let x = cursor.x - (layout.pad + first_row_right) / 2;
        let gap = dip_to_px(CURSOR_GAP_DIP, dpi);
        let below_top = (cursor.y + gap - layout.scene_top).max(work.top);
        let above_bottom = (cursor.y - gap + layout.pad).min(work.bottom);
        let below_height = work.bottom - below_top;
        let above_height = above_bottom - work.top;
        let (y, height, scroll) = if below_height < max_height && above_height > below_height {
            // Keep the scene tiles by the cursor when the popup extends upwards.
            layout = Self::new(dpi, scenes, shortcuts, true);
            let height = max_height.min(above_height).max(1);
            (above_bottom - height, height, layout.content_height - height)
        } else {
            (below_top, max_height.min(below_height).max(1), 0)
        };
        let (x, y) = clamp_and_snap_in_work_area(x, y, width, height, work, dpi, false);
        let bounds = RectData {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        (layout, bounds, scroll)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, PartialEq, Eq)]
pub enum CursorKeyOutcome {
    Ignored,
    Redrawn,
    Dismiss,
    Execute,
}

fn next_focus_hit(
    current: Option<Hit>,
    scenes_len: usize,
    shortcuts_len: usize,
    vk: u16,
) -> Option<Hit> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP};

    let bottom_count = shortcuts_len.max(1);
    let bottom_hit = |idx: usize| -> Hit {
        if shortcuts_len == 0 {
            Hit::Settings
        } else {
            Hit::Shortcut(idx.min(shortcuts_len - 1))
        }
    };
    let first_hit = || -> Hit {
        if scenes_len > 0 {
            Hit::Scene(0)
        } else {
            bottom_hit(0)
        }
    };

    let Some(cur) = current else {
        return Some(first_hit());
    };

    match cur {
        Hit::Scene(i) => {
            if vk == VK_LEFT.0 {
                if i > 0 {
                    Some(Hit::Scene(i - 1))
                } else {
                    Some(bottom_hit(bottom_count - 1))
                }
            } else if vk == VK_RIGHT.0 {
                if i + 1 < scenes_len {
                    Some(Hit::Scene(i + 1))
                } else {
                    Some(bottom_hit(0))
                }
            } else if vk == VK_UP.0 {
                if i >= SCENE_COLUMNS {
                    Some(Hit::Scene(i - SCENE_COLUMNS))
                } else {
                    Some(bottom_hit(bottom_count - 1))
                }
            } else if vk == VK_DOWN.0 {
                if i + SCENE_COLUMNS < scenes_len {
                    Some(Hit::Scene(i + SCENE_COLUMNS))
                } else {
                    Some(bottom_hit(0))
                }
            } else {
                Some(cur)
            }
        }
        Hit::Shortcut(i) => {
            if vk == VK_UP.0 || vk == VK_LEFT.0 {
                if i > 0 {
                    Some(Hit::Shortcut(i - 1))
                } else if scenes_len > 0 {
                    Some(Hit::Scene(scenes_len - 1))
                } else {
                    Some(Hit::Shortcut(bottom_count - 1))
                }
            } else if vk == VK_DOWN.0 || vk == VK_RIGHT.0 {
                if i + 1 < bottom_count {
                    Some(Hit::Shortcut(i + 1))
                } else {
                    Some(first_hit())
                }
            } else {
                Some(cur)
            }
        }
        Hit::Settings => {
            if (vk == VK_UP.0 || vk == VK_LEFT.0 || vk == VK_DOWN.0 || vk == VK_RIGHT.0)
                && scenes_len > 0
            {
                Some(Hit::Scene(0))
            } else {
                Some(Hit::Settings)
            }
        }
    }
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
    scene_wheel: SceneWheel,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    keyboard_focus: Option<Hit>,
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
                layout: PopupLayout::new(96, 0, 0, false),
                window_width: 0,
                width: 0,
                height: 0,
                scroll: 0,
                scene_wheel: SceneWheel::default(),
                hover: None,
                pressed: None,
                keyboard_focus: None,
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
    ) -> windows::core::Result<PopupInput> {
        self.scenes = config.ordered_scenes().into_iter().cloned().collect();
        self.shortcuts.clone_from(&config.shortcuts);
        self.active_scene_id = active_scene_id.map(str::to_owned);
        self.palette = PopupPalette::system();
        if self.dpi != monitor.dpi {
            self.dpi = monitor.dpi;
            self.icon_cache.ensure_dpi(self.dpi);
            self.fonts = PopupFonts::new(self.dpi);
        }

        let (layout, bounds, scroll) = PopupLayout::place(
            cursor,
            monitor,
            self.scenes.len(),
            self.shortcuts.len(),
            config.launcher.cursor_popup_position,
        );
        let width = bounds.width();
        let height = bounds.height();
        self.layout = layout;
        self.scroll = scroll;
        self.scene_wheel = SceneWheel::default();
        self.hover = None;
        self.pressed = None;
        self.keyboard_focus = None;
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
                    nPos: scroll,
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
        Ok(PopupInput {
            rect: bounds,
            corner_radius,
            has_scenes: !self.scenes.is_empty(),
        })
    }

    pub fn hide(&mut self) {
        self.pending_action = None;
        self.keyboard_focus = None;
        if self.visible {
            self.visible = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    fn action_for_hit(&self, hit: Hit) -> PopupAction {
        match hit {
            Hit::Scene(index) => PopupAction::SwitchScene(self.scenes[index].id.clone()),
            Hit::Shortcut(index) => PopupAction::OpenShortcut(self.shortcuts[index].clone()),
            Hit::Settings => PopupAction::OpenSettings,
        }
    }

    fn ensure_hit_visible(&mut self, hit: Hit) {
        let rect = match hit {
            Hit::Scene(index) => self.layout.scene(index, self.width),
            Hit::Shortcut(index) => self.layout.shortcut(index, self.width),
            Hit::Settings => self.layout.shortcut(0, self.width),
        };
        if rect.top < self.scroll {
            self.scroll_to(rect.top);
        } else if rect.bottom > self.scroll + self.height {
            self.scroll_to(rect.bottom - self.height);
        }
    }

    pub fn handle_key(&mut self, vk: u16) -> CursorKeyOutcome {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
        };
        if !self.visible {
            return CursorKeyOutcome::Ignored;
        }
        if vk == VK_ESCAPE.0 {
            return CursorKeyOutcome::Dismiss;
        }
        if vk == VK_RETURN.0 {
            if let Some(hit) = self.keyboard_focus.or(self.hover) {
                self.pending_action = Some(self.action_for_hit(hit));
                return CursorKeyOutcome::Execute;
            }
            return CursorKeyOutcome::Dismiss;
        }
        if matches!(
            vk,
            v if v == VK_UP.0 || v == VK_DOWN.0 || v == VK_LEFT.0 || v == VK_RIGHT.0
        ) {
            let initial = self.keyboard_focus.or_else(|| {
                self.active_scene_id
                    .as_deref()
                    .and_then(|id| self.scenes.iter().position(|s| s.id == id))
                    .map(Hit::Scene)
            });
            let next = next_focus_hit(
                if self.keyboard_focus.is_some() {
                    self.keyboard_focus
                } else {
                    initial.and_then(|h| {
                        // On first arrow press, if active scene exists start from it cleanly
                        if vk == VK_RIGHT.0 || vk == VK_DOWN.0 || vk == VK_LEFT.0 || vk == VK_UP.0 {
                            Some(h)
                        } else {
                            None
                        }
                    })
                },
                self.scenes.len(),
                self.shortcuts.len(),
                vk,
            );
            if next != self.keyboard_focus {
                self.keyboard_focus = next;
                if let Some(hit) = next {
                    self.ensure_hit_visible(hit);
                }
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
            return CursorKeyOutcome::Redrawn;
        }
        CursorKeyOutcome::Ignored
    }

    pub fn wheel_steps(&mut self, delta: i16, mut point: POINT) -> i32 {
        if !self.visible || self.pending_action.is_some() {
            return 0;
        }

        let over_popup = self.layout.content_height > self.height
            && unsafe { ScreenToClient(self.hwnd, &mut point) }.as_bool()
            && (0..self.window_width).contains(&point.x)
            && (0..self.height).contains(&point.y);
        let shortcuts_bottom = self
            .layout
            .shortcut(self.shortcuts.len().max(1) - 1, self.width)
            .bottom;
        let over_shortcuts = (self.layout.shortcut_header_top..shortcuts_bottom)
            .contains(&(point.y + self.scroll));
        let over_scroll_area = over_popup && (point.x >= self.width || over_shortcuts);
        if self.scenes.is_empty() || over_scroll_area {
            self.scene_wheel = SceneWheel::default();
            let pixels = i32::from(delta) * self.layout.row_height / WHEEL_DELTA as i32;
            self.scroll_to(self.scroll - pixels);
            return 0;
        }

        self.scene_wheel.steps(delta)
    }

    pub fn update_active_scene(&mut self, active_scene_id: Option<&str>) {
        if !self.visible || self.active_scene_id.as_deref() == active_scene_id {
            return;
        }
        self.active_scene_id = active_scene_id.map(str::to_owned);
        if let Some(index) = self
            .scenes
            .iter()
            .position(|scene| Some(scene.id.as_str()) == active_scene_id)
        {
            let rect = self.layout.scene(index, self.width);
            if rect.top < self.scroll {
                self.scroll_to(rect.top);
            } else if rect.bottom > self.scroll + self.height {
                self.scroll_to(rect.bottom - self.height);
            }
        }
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
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

    fn scroll_to(&mut self, requested: i32) {
        let scroll = requested.clamp(0, (self.layout.content_height - self.height).max(0));
        if scroll != self.scroll {
            self.scroll = scroll;
            self.hover = None;
            self.pressed = None;
            unsafe {
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
                    popup.keyboard_focus = None;
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
                    popup.pending_action = Some(popup.action_for_hit(hit));
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
                // Also handle native wheel messages if the low-level hook is unavailable.
                if popup.visible {
                    let _ = PostMessageW(popup.owner, WM_APP_CURSOR_POPUP_WHEEL, wparam, lparam);
                }
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
    fn both_popup_positions_fit_on_a_high_dpi_monitor_left_of_primary() {
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
        let place = |cursor, position| PopupLayout::place(cursor, &monitor, 3, 14, position);
        let gap = dip_to_px(CURSOR_GAP_DIP, monitor.dpi);

        let cursor = POINT { x: -800, y: 250 };
        let (layout, bounds, scroll) = place(cursor, CursorPopupPosition::SceneCentered);
        assert_eq!((bounds.left + bounds.right) / 2, cursor.x);
        assert_eq!(bounds.top + layout.scene_top, cursor.y + gap);
        assert_eq!(scroll, 0);

        let cursor = POINT { x: -1910, y: -90 };
        let (_, bounds, _) = place(cursor, CursorPopupPosition::SceneCentered);
        assert_eq!(bounds.left, monitor.work_area.left);
        assert_eq!(bounds.top, monitor.work_area.top);

        let cursor = POINT { x: -3, y: 965 };
        let (layout, bounds, scroll) = place(cursor, CursorPopupPosition::BesideCursor);
        assert_eq!(bounds.right, cursor.x - gap);
        assert_eq!(bounds.bottom, monitor.work_area.bottom);
        assert!(layout.scene_top < layout.shortcut_top);
        assert_eq!(scroll, 0);

        let (layout, bounds, scroll) = place(cursor, CursorPopupPosition::SceneCentered);
        assert_eq!(bounds.right, monitor.work_area.right);
        assert_eq!(bounds.bottom, monitor.work_area.bottom);
        assert!(layout.shortcut_top < layout.scene_header_top);
        assert!(scroll > 0);
        let tile_bottom = bounds.top + layout.scene(2, bounds.width()).bottom - scroll;
        assert_eq!(tile_bottom, bounds.bottom - layout.pad);
    }

    #[test]
    fn native_popup_routes_wheel_and_click_without_taking_focus() {
        use windows::Win32::Graphics::Gdi::ClientToScreen;

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
                x: work_area.left + work_area.width() / 2,
                y: work_area.bottom - 40,
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

            assert!(popup.scroll > 0);
            let mut scrollbar = SCROLLINFO {
                cbSize: size_of::<SCROLLINFO>() as u32,
                fMask: SIF_POS,
                ..Default::default()
            };
            GetScrollInfo(popup.hwnd, SB_VERT, &mut scrollbar).unwrap();
            assert_eq!(scrollbar.nPos, popup.scroll);
            let initial_scroll = popup.scroll;
            assert_eq!(popup.wheel_steps(-120, cursor), 1);
            assert_eq!(popup.scroll, initial_scroll);
            let x = cursor.x - bounds.rect.left;
            let y = cursor.y - bounds.rect.top - CURSOR_GAP_DIP - 1;
            let hit = LPARAM(((y as isize) << 16) | x as isize);
            assert!(popup.hit_test(hit) == Some(Hit::Scene(1)));

            let _ = SendMessageW(
                popup.hwnd,
                WM_VSCROLL,
                WPARAM(SB_TOP.0 as usize),
                LPARAM(0),
            );
            assert_eq!(popup.scroll, 0);
            let row = popup
                .viewport_rect(popup.layout.shortcut(0, popup.width))
                .unwrap();
            let hit = LPARAM((((row.top + 3) as isize) << 16) | (row.left + 3) as isize);
            let _ = SendMessageW(popup.hwnd, WM_LBUTTONDOWN, WPARAM(0), hit);
            let _ = SendMessageW(popup.hwnd, WM_LBUTTONUP, WPARAM(0), hit);
            assert!(
                matches!(popup.pending_action.take(), Some(PopupAction::OpenShortcut(shortcut))
                if shortcut == config.shortcuts[0])
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

            let mut shortcut_point = POINT {
                x: row.left + 3,
                y: row.top + 3,
            };
            assert!(ClientToScreen(popup.hwnd, &mut shortcut_point).as_bool());
            assert_eq!(popup.wheel_steps(-120, shortcut_point), 0);
            assert_eq!(popup.scroll, popup.layout.row_height);

            popup.update_active_scene(Some("trade"));
            assert_eq!(popup.active_scene_id.as_deref(), Some("trade"));
            let active_tile = popup.viewport_rect(popup.layout.scene(0, popup.width)).unwrap();
            assert!(active_tile.top >= 0 && active_tile.bottom <= popup.height);
            assert!(popup.visible);
            assert_ne!(GetForegroundWindow(), popup.hwnd);
            popup.hide();
            assert!(!IsWindowVisible(popup.hwnd).as_bool());
            drop(popup);
            DestroyWindow(owner).unwrap();
        }
    }

    #[test]
    fn keyboard_focus_moves_across_scenes_and_shortcuts() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP};

        // 4 scenes (3x2 grid, bottom row has 1 item) + 2 shortcuts
        assert_eq!(next_focus_hit(None, 4, 2, VK_DOWN.0), Some(Hit::Scene(0)));
        assert_eq!(
            next_focus_hit(Some(Hit::Scene(0)), 4, 2, VK_RIGHT.0),
            Some(Hit::Scene(1))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Scene(0)), 4, 2, VK_DOWN.0),
            Some(Hit::Scene(3))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Scene(3)), 4, 2, VK_DOWN.0),
            Some(Hit::Shortcut(0))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Shortcut(0)), 4, 2, VK_DOWN.0),
            Some(Hit::Shortcut(1))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Shortcut(1)), 4, 2, VK_DOWN.0),
            Some(Hit::Scene(0))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Shortcut(0)), 4, 2, VK_UP.0),
            Some(Hit::Scene(3))
        );
        assert_eq!(
            next_focus_hit(Some(Hit::Scene(0)), 4, 2, VK_LEFT.0),
            Some(Hit::Shortcut(1))
        );
        // Empty shortcuts falls back to Settings row
        assert_eq!(
            next_focus_hit(Some(Hit::Scene(2)), 3, 0, VK_DOWN.0),
            Some(Hit::Settings)
        );
    }
}
