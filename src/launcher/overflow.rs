//! Custom non-activating (`WS_EX_NOACTIVATE`) overflow popup window for `⋯` scenes.
//!
//! Per Section 2.1, 2.3, AT-11, and AT-29 of spec.md:
//! - Standard Win32 `TrackPopupMenu` requires the owner window to take foreground focus to close on outside click.
//! - Because scene switching must NEVER steal foreground keyboard focus from trading/coding apps,
//!   the `⋯` popup is implemented as a custom `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`
//!   window that returns `MA_NOACTIVATE` on `WM_MOUSEACTIVATE`.
//! - Opens in the direction that fits inside the monitor's `work_area`.
//! - Closes automatically on item selection, clicking `⋯` again, or a short one-shot timer after pointer leave.

use crate::config::{RectData, Scene};
use crate::launcher::input::CORNER_RADIUS_DIP;
use crate::launcher::paint::{
    colors, draw_rounded_rect_border_gdiplus, draw_scene_icon, draw_variant_badge,
    fill_rounded_rect_gdiplus, GdiThemeCache, GdiplusContext,
};
use crate::launcher::variant_fan::{FanAxis, FanOutcome, VariantFanHost, HOLD_OPEN_DELAY_MS};
use crate::platform::monitors::dip_to_px;

/// Custom message sent to the main launcher HWND when an overflow scene item is selected.
/// `WPARAM` contains the index into the current `overflow_scenes` list.
/// `LPARAM` is `0` for the preferred/default variant, or `variant_index + 1` for a specific variant.
pub const WM_APP_OVERFLOW_SELECT: u32 = 0x8000 + 3; // WM_APP + 3

const TIMER_OVERFLOW_MOUSE_CHECK: usize = 301;
const TIMER_OVERFLOW_FAN_HOLD: usize = 302;
const ROW_HEIGHT_DIP: i32 = 34;
const POPUP_WIDTH_DIP: i32 = 176;
const POPUP_PAD_DIP: i32 = 5;

/// Compute the screen rectangle for the overflow popup so it stays within `work_area` (Section 2.3).
pub fn compute_overflow_popup_rect(
    anchor_screen_rect: RectData,
    item_count: usize,
    dpi: u32,
    work_area: RectData,
) -> RectData {
    let pad = dip_to_px(POPUP_PAD_DIP, dpi);
    let row_h = dip_to_px(ROW_HEIGHT_DIP, dpi);
    let width = dip_to_px(POPUP_WIDTH_DIP, dpi);
    let height = pad * 2 + (item_count.max(1) as i32) * row_h;
    let gap = dip_to_px(4, dpi);

    // Prefer opening below the anchor button; if it overflows bottom of work_area, open above
    let mut y = anchor_screen_rect.bottom + gap;
    if y + height > work_area.bottom && anchor_screen_rect.top - gap - height >= work_area.top {
        y = anchor_screen_rect.top - gap - height;
    }
    y = y.clamp(
        work_area.top,
        (work_area.bottom - height).max(work_area.top),
    );

    // Prefer aligning left edge with anchor button; clamp to right edge of work_area
    let mut x = anchor_screen_rect.left;
    if x + width > work_area.right {
        x = anchor_screen_rect.right - width;
    }
    x = x.clamp(work_area.left, (work_area.right - width).max(work_area.left));

    RectData {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

#[cfg(windows)]
pub struct OverflowPopup {
    hwnd: windows::Win32::Foundation::HWND,
    parent_hwnd: windows::Win32::Foundation::HWND,
    scenes: Vec<Scene>,
    active_scene_id: Option<String>,
    hovered_idx: Option<usize>,
    pressed: Option<usize>,
    tracking_leave: bool,
    dpi: u32,
    font_dpi: u32,
    font: windows::Win32::Graphics::Gdi::HFONT,
    visible: bool,
    work_area: RectData,
    theme: GdiThemeCache,
    variant_fan: VariantFanHost,
}

#[cfg(windows)]
impl Drop for OverflowPopup {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        unsafe {
            if !self.font.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}

#[cfg(windows)]
unsafe fn create_popup_font(dpi: u32) -> windows::Win32::Graphics::Gdi::HFONT {
    use windows::core::w;
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_DONTCARE,
        FW_NORMAL, OUT_DEFAULT_PRECIS,
    };
    CreateFontW(
        -dip_to_px(13, dpi.max(96)),
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        FF_DONTCARE.0 as u32,
        w!("Segoe UI Variable Text"),
    )
}

#[cfg(windows)]
impl OverflowPopup {
    pub fn create(
        parent_hwnd: windows::Win32::Foundation::HWND,
        gdiplus: GdiplusContext,
    ) -> Result<Box<Self>, String> {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowLongPtrW, CS_DROPSHADOW,
            CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, WNDCLASSW, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };

        static REGISTER_ONCE: Once = Once::new();
        let class_name = w!("KiriOverflowPopupClass");

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
                    lpfnWndProc: Some(overflow_wndproc),
                    hInstance: hinstance.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let mut popup = Box::new(Self {
                hwnd: HWND::default(),
                parent_hwnd,
                scenes: Vec::new(),
                active_scene_id: None,
                hovered_idx: None,
                pressed: None,
                tracking_leave: false,
                dpi: 96,
                font_dpi: 96,
                font: create_popup_font(96),
                visible: false,
                work_area: RectData::default(),
                theme: GdiThemeCache::with_context(96, gdiplus.clone()),
                variant_fan: VariantFanHost::new(gdiplus),
            });

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!(""),
                WS_POPUP,
                0,
                0,
                10,
                10,
                Some(parent_hwnd),
                None,
                Some(hinstance.into()),
                None,
            )
            .map_err(|e| format!("Failed to create overflow popup: {}", e))?;

            popup.hwnd = hwnd;
            let ptr = &mut *popup as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            Ok(popup)
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn hide(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, ShowWindow, SW_HIDE};
        if self.variant_fan.is_active() {
            self.variant_fan.cancel();
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if !self.visible {
            return;
        }
        self.visible = false;
        self.hovered_idx = None;
        self.pressed = None;
        self.tracking_leave = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_OVERFLOW_FAN_HOLD);
            let _ = KillTimer(Some(self.hwnd), TIMER_OVERFLOW_MOUSE_CHECK);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn toggle_or_show(
        &mut self,
        scenes: Vec<Scene>,
        active_scene_id: Option<String>,
        anchor_screen_rect: RectData,
        work_area: RectData,
        dpi: u32,
    ) {
        use windows::Win32::Graphics::Gdi::{
            CreateRoundRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, HGDIOBJ,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_SHOWNOACTIVATE,
        };

        if self.visible {
            self.hide();
            return;
        }
        if scenes.is_empty() {
            return;
        }

        let rc = compute_overflow_popup_rect(anchor_screen_rect, scenes.len(), dpi, work_area);
        self.scenes = scenes;
        self.active_scene_id = active_scene_id;
        self.work_area = work_area;
        self.dpi = dpi.max(96);
        self.theme.ensure_dpi(self.dpi);
        if self.font_dpi != self.dpi {
            unsafe {
                let new_font = create_popup_font(self.dpi);
                if !self.font.0.is_null() {
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                        windows::Win32::Graphics::Gdi::HGDIOBJ(self.font.0),
                    );
                }
                self.font = new_font;
                self.font_dpi = self.dpi;
            }
        }
        self.hovered_idx = None;
        self.pressed = None;
        self.tracking_leave = false;
        self.visible = true;

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                rc.left,
                rc.top,
                rc.width(),
                rc.height(),
                SWP_NOACTIVATE,
            );
            let diameter = dip_to_px(CORNER_RADIUS_DIP * 2, self.dpi);
            let rgn = CreateRoundRectRgn(0, 0, rc.width() + 1, rc.height() + 1, diameter, diameter);
            if !rgn.is_invalid() && SetWindowRgn(self.hwnd, Some(rgn), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(rgn.0));
            }
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            // Fallback outside-close timer while popup is open
            let _ = SetTimer(Some(self.hwnd), TIMER_OVERFLOW_MOUSE_CHECK, 350, None);
        }
    }

    fn row_at_y(&self, y: i32) -> Option<usize> {
        let pad = dip_to_px(POPUP_PAD_DIP, self.dpi);
        let row_h = dip_to_px(ROW_HEIGHT_DIP, self.dpi).max(1);
        if y < pad {
            return None;
        }
        let idx = ((y - pad) / row_h) as usize;
        if idx < self.scenes.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn row_screen_rect(&self, index: usize) -> Option<RectData> {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        if index >= self.scenes.len() {
            return None;
        }
        let mut win_rc = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut win_rc).ok()? };
        let pad = dip_to_px(POPUP_PAD_DIP, self.dpi);
        let row_h = dip_to_px(ROW_HEIGHT_DIP, self.dpi);
        let width = dip_to_px(POPUP_WIDTH_DIP, self.dpi);
        let top = win_rc.top + pad + index as i32 * row_h;
        Some(RectData {
            left: win_rc.left + pad,
            top,
            right: win_rc.left + width - pad,
            bottom: top + row_h,
        })
    }

    fn hit_test(&self, lparam: windows::Win32::Foundation::LPARAM) -> Option<usize> {
        let x = lparam.0 as i16 as i32;
        let y = (lparam.0 >> 16) as i16 as i32;
        let width = dip_to_px(POPUP_WIDTH_DIP, self.dpi);
        let pad = dip_to_px(POPUP_PAD_DIP, self.dpi);
        if x < pad || x >= width - pad {
            return None;
        }
        self.row_at_y(y)
    }
}

#[cfg(windows)]
unsafe extern "system" fn overflow_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{COLORREF, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, SelectObject, SetBkMode,
        SetTextColor, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
        DT_SINGLELINE, DT_VCENTER, HGDIOBJ, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::Graphics::GdiPlus::{
        GdipCreateFromHDC, GdipDeleteGraphics, GdipSetInterpolationMode, GdipSetSmoothingMode,
        GpGraphics, InterpolationModeHighQualityBicubic, Ok as GpOk, SmoothingModeAntiAlias,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
        KillTimer, PostMessageW, SetTimer, GWLP_USERDATA, MA_NOACTIVATE, WM_CAPTURECHANGED,
        WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER,
    };
    const WM_MOUSELEAVE: u32 = 0x02A3;

    let res = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverflowPopup;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let popup = &mut *ptr;

        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => LRESULT(0),
            WM_MOUSEMOVE => {
                if popup.variant_fan.is_active() {
                    let mut pt = POINT::default();
                    if GetCursorPos(&mut pt).is_ok() {
                        if matches!(popup.variant_fan.on_move(pt.x, pt.y), FanOutcome::Opened) {
                            let _ = KillTimer(Some(hwnd), TIMER_OVERFLOW_FAN_HOLD);
                        }
                    }
                    return LRESULT(0);
                }
                if !popup.tracking_leave {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    popup.tracking_leave = TrackMouseEvent(&mut tme).is_ok();
                }
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let new_hover = popup.row_at_y(y);
                if new_hover != popup.hovered_idx {
                    popup.hovered_idx = new_hover;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                popup.tracking_leave = false;
                if popup.variant_fan.is_active() {
                    return LRESULT(0);
                }
                let had_press = popup.pressed.take().is_some();
                if popup.hovered_idx.take().is_some() || had_press {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if popup.variant_fan.is_sticky() {
                    let mut pt = POINT::default();
                    if GetCursorPos(&mut pt).is_ok()
                        && matches!(popup.variant_fan.on_down(pt.x, pt.y), FanOutcome::Cancelled)
                    {
                        let _ = ReleaseCapture();
                        popup.pressed = None;
                        let mut win_rc = RECT::default();
                        if GetWindowRect(hwnd, &mut win_rc).is_ok()
                            && (pt.x < win_rc.left
                                || pt.x >= win_rc.right
                                || pt.y < win_rc.top
                                || pt.y >= win_rc.bottom)
                        {
                            popup.hide();
                        } else {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                    return LRESULT(0);
                }
                let hit = popup.hit_test(lparam);
                popup.pressed = hit;
                if let Some(idx) = hit {
                    if popup.scenes[idx].variants.len() > 1 {
                        if let Some(anchor) = popup.row_screen_rect(idx) {
                            let mut pt = POINT::default();
                            if GetCursorPos(&mut pt).is_ok()
                                && popup.variant_fan.begin_press(
                                    &popup.scenes[idx],
                                    anchor,
                                    popup.work_area,
                                    FanAxis::Horizontal,
                                    popup.dpi,
                                    pt.x,
                                    pt.y,
                                )
                            {
                                let _ = SetCapture(hwnd);
                                let _ = SetTimer(
                                    Some(hwnd),
                                    TIMER_OVERFLOW_FAN_HOLD,
                                    HOLD_OPEN_DELAY_MS,
                                    None,
                                );
                            }
                        }
                    }
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP => {
                if popup.variant_fan.is_active() {
                    let _ = KillTimer(Some(hwnd), TIMER_OVERFLOW_FAN_HOLD);
                    let _ = popup.variant_fan.cancel();
                    let _ = ReleaseCapture();
                    popup.pressed = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                if popup.variant_fan.is_active() {
                    let _ = KillTimer(Some(hwnd), TIMER_OVERFLOW_FAN_HOLD);
                    let _ = popup.variant_fan.cancel();
                    popup.pressed = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if popup.variant_fan.is_active() {
                    let _ = KillTimer(Some(hwnd), TIMER_OVERFLOW_FAN_HOLD);
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    match popup.variant_fan.on_up(pt.x, pt.y) {
                        FanOutcome::EnteredSticky => return LRESULT(0),
                        FanOutcome::ClickDefault { .. } => {
                            let _ = ReleaseCapture();
                            let pressed = popup.pressed.take();
                            if let Some(idx) =
                                popup.hit_test(lparam).filter(|hit| Some(*hit) == pressed)
                            {
                                let parent = popup.parent_hwnd;
                                popup.hide();
                                let _ = PostMessageW(
                                    Some(parent),
                                    WM_APP_OVERFLOW_SELECT,
                                    WPARAM(idx),
                                    windows::Win32::Foundation::LPARAM(0),
                                );
                                return LRESULT(0);
                            }
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            return LRESULT(0);
                        }
                        FanOutcome::CommitVariant {
                            scene_id,
                            variant_id,
                        } => {
                            let _ = ReleaseCapture();
                            popup.pressed = None;
                            if let Some((idx, scene)) = popup
                                .scenes
                                .iter()
                                .enumerate()
                                .find(|(_, s)| s.id == scene_id)
                            {
                                let v_param = scene
                                    .variants
                                    .iter()
                                    .position(|v| v.id == variant_id)
                                    .map_or(0, |vi| (vi + 1) as isize);
                                let parent = popup.parent_hwnd;
                                popup.hide();
                                let _ = PostMessageW(
                                    Some(parent),
                                    WM_APP_OVERFLOW_SELECT,
                                    WPARAM(idx),
                                    windows::Win32::Foundation::LPARAM(v_param),
                                );
                                return LRESULT(0);
                            }
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            return LRESULT(0);
                        }
                        FanOutcome::Cancelled => {
                            let _ = ReleaseCapture();
                            popup.pressed = None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            return LRESULT(0);
                        }
                        FanOutcome::None | FanOutcome::Opened | FanOutcome::Updated => {
                            return LRESULT(0);
                        }
                    }
                }
                let pressed = popup.pressed.take();
                if let Some(idx) = popup.hit_test(lparam).filter(|hit| Some(*hit) == pressed) {
                    let parent = popup.parent_hwnd;
                    popup.hide();
                    let _ = PostMessageW(
                        Some(parent),
                        WM_APP_OVERFLOW_SELECT,
                        WPARAM(idx),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                    return LRESULT(0);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == TIMER_OVERFLOW_FAN_HOLD {
                    let _ = KillTimer(Some(hwnd), TIMER_OVERFLOW_FAN_HOLD);
                    let mut pt = POINT::default();
                    if GetCursorPos(&mut pt).is_ok() {
                        let _ = popup.variant_fan.on_hold_timer(pt.x, pt.y);
                    }
                    return LRESULT(0);
                }
                if wparam.0 == TIMER_OVERFLOW_MOUSE_CHECK
                    && popup.visible
                    && !popup.variant_fan.is_active()
                {
                    let mut pt = POINT::default();
                    let mut win_rc = RECT::default();
                    if GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut win_rc).is_ok() {
                        let margin = dip_to_px(28, popup.dpi);
                        let inside = pt.x >= win_rc.left - margin
                            && pt.x <= win_rc.right + margin
                            && pt.y >= win_rc.top - margin
                            && pt.y <= win_rc.bottom + margin;
                        if !inside {
                            popup.hide();
                        }
                    }
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut client_rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut client_rc);
                let w = (client_rc.right - client_rc.left).max(1);
                let h = (client_rc.bottom - client_rc.top).max(1);

                let mem_dc = CreateCompatibleDC(Some(hdc));
                let mem_bmp = CreateCompatibleBitmap(hdc, w, h);
                let old_bmp = SelectObject(mem_dc, HGDIOBJ(mem_bmp.0));
                let font_obj = if !popup.font.0.is_null() {
                    HGDIOBJ(popup.font.0)
                } else {
                    GetStockObject(DEFAULT_GUI_FONT)
                };
                let old_font = SelectObject(mem_dc, font_obj);

                FillRect(mem_dc, &client_rc, popup.theme.bg_brush);
                let _ = SetBkMode(mem_dc, TRANSPARENT);

                let pad = dip_to_px(POPUP_PAD_DIP, popup.dpi);
                let row_h = dip_to_px(ROW_HEIGHT_DIP, popup.dpi);
                let scale = popup.dpi as f32 / 96.0;
                let outer_r = (CORNER_RADIUS_DIP as f32 * scale).max(4.0);
                let row_r = ((CORNER_RADIUS_DIP - 1) as f32 * scale).max(3.5);
                let stroke = (1.0 * scale).max(1.0);

                // First pass (GDI+): rounded border, rounded row fills, and antialiased scene icons
                let mut graphics: *mut GpGraphics = std::ptr::null_mut();
                if GdipCreateFromHDC(mem_dc, &mut graphics) == GpOk && !graphics.is_null() {
                    let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
                    let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);

                    draw_rounded_rect_border_gdiplus(
                        graphics,
                        0.5,
                        0.5,
                        (w as f32 - 1.0).max(1.0),
                        (h as f32 - 1.0).max(1.0),
                        outer_r,
                        colors::BORDER_ARGB,
                        stroke,
                    );

                    for (idx, scene) in popup.scenes.iter().enumerate() {
                        let top = pad + (idx as i32) * row_h;
                        let row_left = pad;
                        let row_right = client_rc.right - pad;
                        let row_w = (row_right - row_left).max(1) as f32;
                        let row_hf = (row_h - dip_to_px(2, popup.dpi)).max(1) as f32;
                        let row_y = (top + dip_to_px(1, popup.dpi)) as f32;

                        let is_active =
                            popup.active_scene_id.as_deref() == Some(scene.id.as_str());
                        let is_hovered = popup.hovered_idx == Some(idx);
                        let is_pressed = popup.pressed == Some(idx);

                        if is_active {
                            let fill = if is_pressed {
                                colors::BTN_ACTIVE_PRESSED_ARGB
                            } else {
                                colors::BTN_ACTIVE_ARGB
                            };
                            fill_rounded_rect_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                fill,
                            );
                            draw_rounded_rect_border_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                colors::BTN_ACTIVE_BORDER_ARGB,
                                stroke,
                            );
                        } else if is_pressed {
                            fill_rounded_rect_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                colors::BTN_PRESSED_ARGB,
                            );
                            draw_rounded_rect_border_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                colors::BORDER_ARGB,
                                stroke,
                            );
                        } else if is_hovered {
                            fill_rounded_rect_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                colors::BTN_HOVER_ARGB,
                            );
                            draw_rounded_rect_border_gdiplus(
                                graphics,
                                row_left as f32 + 0.5,
                                row_y + 0.5,
                                row_w - 1.0,
                                row_hf - 1.0,
                                row_r,
                                colors::BORDER_ARGB,
                                stroke,
                            );
                        }

                        let icon_box = RectData {
                            left: row_left + dip_to_px(5, popup.dpi),
                            top,
                            right: row_left + dip_to_px(29, popup.dpi),
                            bottom: top + row_h,
                        };
                        draw_scene_icon(
                            graphics,
                            &mut popup.theme,
                            &icon_box,
                            &scene.icon,
                            &scene.name,
                            is_active,
                            popup.dpi,
                        );
                        if scene.variants.len() > 1 {
                            let badge_rc = RectData {
                                left: row_right - dip_to_px(18, popup.dpi),
                                top,
                                right: row_right - dip_to_px(2, popup.dpi),
                                bottom: top + row_h,
                            };
                            let badge_color = if is_active {
                                0xDDEAF2FF
                            } else {
                                0xB88C96A8
                            };
                            draw_variant_badge(graphics, badge_rc, badge_color, popup.dpi);
                        }
                    }

                    let _ = GdipDeleteGraphics(graphics);
                }

                // Second pass (GDI ClearType): scene labels & active checkmark
                for (idx, scene) in popup.scenes.iter().enumerate() {
                    let top = pad + (idx as i32) * row_h;
                    let is_active = popup.active_scene_id.as_deref() == Some(scene.id.as_str());
                    let text_rgb = if is_active {
                        COLORREF(0x00FF_FFFF)
                    } else {
                        COLORREF(0x00F7_EFEA) // rgb(234, 239, 247) in BGR
                    };
                    let _ = SetTextColor(mem_dc, text_rgb);

                    let right_pad = if is_active || scene.variants.len() > 1 {
                        dip_to_px(24, popup.dpi)
                    } else {
                        dip_to_px(8, popup.dpi)
                    };
                    let content_right = client_rc.right - pad;
                    let mut text_rc = RECT {
                        left: pad + dip_to_px(32, popup.dpi),
                        top,
                        right: content_right - right_pad,
                        bottom: top + row_h,
                    };
                    let mut wide: Vec<u16> = scene.name.encode_utf16().collect();
                    let _ = DrawTextW(
                        mem_dc,
                        &mut wide,
                        &mut text_rc,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );

                    if is_active {
                        let _ = SetTextColor(mem_dc, COLORREF(0x00FF_D8BD)); // soft accent blue
                        let mut check_rc = RECT {
                            left: content_right - dip_to_px(22, popup.dpi),
                            top,
                            right: content_right - dip_to_px(8, popup.dpi),
                            bottom: top + row_h,
                        };
                        let mut chk: Vec<u16> = "✓".encode_utf16().collect();
                        let _ = DrawTextW(
                            mem_dc,
                            &mut chk,
                            &mut check_rc,
                            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                        );
                    }
                }

                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem_dc), 0, 0, SRCCOPY);
                let _ = SelectObject(mem_dc, old_font);
                let _ = SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(mem_bmp.0));
                let _ = DeleteDC(mem_dc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    res.unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}
