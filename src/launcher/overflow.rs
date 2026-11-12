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
use crate::platform::monitors::dip_to_px;

/// Custom message sent to the main launcher HWND when an overflow scene item is clicked.
/// `WPARAM` contains the index into the current `overflow_scenes` list.
pub const WM_APP_OVERFLOW_SELECT: u32 = 0x8000 + 3; // WM_APP + 3

const TIMER_OVERFLOW_MOUSE_CHECK: usize = 301;
const ROW_HEIGHT_DIP: i32 = 30;
const POPUP_WIDTH_DIP: i32 = 148;
const POPUP_PAD_DIP: i32 = 4;

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
    dpi: u32,
    font_dpi: u32,
    font: windows::Win32::Graphics::Gdi::HFONT,
    visible: bool,
    bg_brush: windows::Win32::Graphics::Gdi::HBRUSH,
    border_brush: windows::Win32::Graphics::Gdi::HBRUSH,
    hover_brush: windows::Win32::Graphics::Gdi::HBRUSH,
    active_brush: windows::Win32::Graphics::Gdi::HBRUSH,
}

#[cfg(windows)]
impl Drop for OverflowPopup {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        unsafe {
            if !self.font.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.border_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.hover_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.active_brush.0));
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
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        FF_DONTCARE.0 as u32,
        w!("Yu Gothic UI"),
    )
}

#[cfg(windows)]
impl OverflowPopup {
    pub fn create(parent_hwnd: windows::Win32::Foundation::HWND) -> Result<Box<Self>, String> {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::Foundation::{COLORREF, HWND};
        use windows::Win32::Graphics::Gdi::CreateSolidBrush;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowLongPtrW, CS_HREDRAW,
            CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST, WS_POPUP,
        };

        static REGISTER_ONCE: Once = Once::new();
        let class_name = w!("KiriOverflowPopupClass");

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
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
                dpi: 96,
                font_dpi: 96,
                font: create_popup_font(96),
                visible: false,
                bg_brush: CreateSolidBrush(COLORREF(0x001F_1A18)),
                border_brush: CreateSolidBrush(COLORREF(0x0044_3934)),
                hover_brush: CreateSolidBrush(COLORREF(0x0038_2E2A)),
                active_brush: CreateSolidBrush(COLORREF(0x008A_4C21)),
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
                parent_hwnd,
                None,
                hinstance,
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
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, ShowWindow, SW_HIDE};
        if !self.visible {
            return;
        }
        self.visible = false;
        self.hovered_idx = None;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_OVERFLOW_MOUSE_CHECK);
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
        use windows::Win32::Graphics::Gdi::InvalidateRect;
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
        self.dpi = dpi.max(96);
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
        self.visible = true;

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rc.left,
                rc.top,
                rc.width(),
                rc.height(),
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(self.hwnd, None, false);
            // Single low-overhead check while popup is open to close after pointer leaves
            let _ = SetTimer(self.hwnd, TIMER_OVERFLOW_MOUSE_CHECK, 350, None);
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
        DrawTextW, EndPaint, FillRect, FrameRect, GetStockObject, InvalidateRect, SelectObject,
        SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HGDIOBJ,
        PAINTSTRUCT, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
        PostMessageW, GWLP_USERDATA, MA_NOACTIVATE, WM_DPICHANGED, WM_LBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE, WM_PAINT, WM_TIMER,
    };

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
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let new_hover = popup.row_at_y(y);
                if new_hover != popup.hovered_idx {
                    popup.hovered_idx = new_hover;
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if let Some(idx) = popup.row_at_y(y) {
                    let parent = popup.parent_hwnd;
                    popup.hide();
                    let _ = PostMessageW(
                        parent,
                        WM_APP_OVERFLOW_SELECT,
                        WPARAM(idx),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == TIMER_OVERFLOW_MOUSE_CHECK && popup.visible {
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

                let mem_dc = CreateCompatibleDC(hdc);
                let mem_bmp = CreateCompatibleBitmap(hdc, w, h);
                let old_bmp = SelectObject(mem_dc, HGDIOBJ(mem_bmp.0));
                let font_obj = if !popup.font.0.is_null() {
                    HGDIOBJ(popup.font.0)
                } else {
                    GetStockObject(DEFAULT_GUI_FONT)
                };
                let old_font = SelectObject(mem_dc, font_obj);

                FillRect(mem_dc, &client_rc, popup.bg_brush);
                FrameRect(mem_dc, &client_rc, popup.border_brush);
                let _ = SetBkMode(mem_dc, TRANSPARENT);

                let pad = dip_to_px(POPUP_PAD_DIP, popup.dpi);
                let row_h = dip_to_px(ROW_HEIGHT_DIP, popup.dpi);

                for (idx, scene) in popup.scenes.iter().enumerate() {
                    let top = pad + (idx as i32) * row_h;
                    let row_rc = RECT {
                        left: pad,
                        top,
                        right: client_rc.right - pad,
                        bottom: top + row_h,
                    };
                    let is_active = popup.active_scene_id.as_deref() == Some(scene.id.as_str());
                    if is_active {
                        FillRect(mem_dc, &row_rc, popup.active_brush);
                    } else if popup.hovered_idx == Some(idx) {
                        FillRect(mem_dc, &row_rc, popup.hover_brush);
                    }

                    let _ = SetTextColor(mem_dc, COLORREF(0x00E6_E1DC));
                    let mut text_rc = RECT {
                        left: row_rc.left + dip_to_px(10, popup.dpi),
                        top: row_rc.top,
                        right: row_rc.right - dip_to_px(6, popup.dpi),
                        bottom: row_rc.bottom,
                    };
                    let mut wide: Vec<u16> = scene.name.encode_utf16().collect();
                    let _ = DrawTextW(
                        mem_dc,
                        &mut wide,
                        &mut text_rc,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                    );
                }

                let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);
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
