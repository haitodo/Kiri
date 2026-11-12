//! Double-buffered GDI rendering and cached GDI resources (`paint.rs`).
//!
//! Per Section 2.1 & 7.3 of spec.md:
//! - Reuses cached `HBRUSH` / `HPEN` objects via `GdiThemeCache` and releases them in `Drop`.
//! - Uses off-screen memory DC (`CreateCompatibleDC` + `CreateCompatibleBitmap`) to eliminate flicker.
//! - Draws crisp DPI-scaled icons for `"chart"` (Trade), `"code"` (Code), `"globe"` (Web),
//!   and `"⋯"` (Overflow), plus custom `.ico` file loading when an icon path is configured.
//! - Clearly distinguishes `Active(scene)` (accent highlight + border) vs `Unknown` (unselected,
//!   with an optional subtle underline dot for `last_known_scene_id`).

use crate::config::RectData;
use crate::launcher::input::{HitTarget, LauncherLayout};
use crate::platform::monitors::dip_to_px;
use crate::scenes::manager::SceneState;
use std::collections::HashMap;

#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, Ellipse, FillRect, FrameRect, LineTo, MoveToEx, RoundRect, SelectObject,
    SetBkMode, SetTextColor, HBRUSH, HDC, HGDIOBJ, HPEN, PS_SOLID, SRCCOPY, TRANSPARENT,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, LoadImageW, DI_NORMAL, HICON, IMAGE_ICON, LR_LOADFROMFILE,
};

#[cfg(windows)]
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// Cached GDI brushes and pens so we never allocate/leak GDI objects per `WM_PAINT` (Section 7.3).
#[cfg(windows)]
pub struct GdiThemeCache {
    pub bg_brush: HBRUSH,
    pub border_brush: HBRUSH,
    pub btn_hover_brush: HBRUSH,
    pub btn_active_brush: HBRUSH,
    pub btn_active_border_brush: HBRUSH,
    pub grip_dot_brush: HBRUSH,
    pub subtle_hint_brush: HBRUSH,
    pub warn_dot_brush: HBRUSH,
    pub icon_pen_normal: HPEN,
    pub icon_pen_active: HPEN,
    pub cached_dpi: u32,
    pub icon_cache: HashMap<String, Option<HICON>>,
}

#[cfg(windows)]
impl GdiThemeCache {
    pub fn new(dpi: u32) -> Self {
        unsafe {
            let stroke_w = dip_to_px(2, dpi).max(1);
            Self {
                bg_brush: CreateSolidBrush(rgb(24, 26, 31)),
                border_brush: CreateSolidBrush(rgb(52, 57, 68)),
                btn_hover_brush: CreateSolidBrush(rgb(42, 46, 56)),
                btn_active_brush: CreateSolidBrush(rgb(33, 76, 138)),
                btn_active_border_brush: CreateSolidBrush(rgb(76, 148, 246)),
                grip_dot_brush: CreateSolidBrush(rgb(110, 118, 135)),
                subtle_hint_brush: CreateSolidBrush(rgb(90, 115, 155)),
                warn_dot_brush: CreateSolidBrush(rgb(240, 160, 40)),
                icon_pen_normal: CreatePen(PS_SOLID, stroke_w, rgb(205, 212, 224)),
                icon_pen_active: CreatePen(PS_SOLID, stroke_w, rgb(255, 255, 255)),
                cached_dpi: dpi,
                icon_cache: HashMap::new(),
            }
        }
    }

    pub fn ensure_dpi(&mut self, dpi: u32) {
        if self.cached_dpi != dpi {
            for (_, icon_opt) in self.icon_cache.drain() {
                if let Some(icon) = icon_opt {
                    unsafe {
                        let _ = DestroyIcon(icon);
                    }
                }
            }
            *self = Self::new(dpi);
        }
    }

    pub fn get_or_load_icon(&mut self, icon_source: &str, dpi: u32) -> Option<HICON> {
        // Built-in vector icons never touch the filesystem
        if icon_source.is_empty() || matches!(icon_source, "chart" | "code" | "globe") {
            return None;
        }
        if let Some(&cached) = self.icon_cache.get(icon_source) {
            return cached;
        }

        let path = std::path::Path::new(icon_source);
        let loaded = if path.exists() {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let size = dip_to_px(18, dpi);
            unsafe {
                LoadImageW(
                    None,
                    windows::core::PCWSTR(wide.as_ptr()),
                    IMAGE_ICON,
                    size,
                    size,
                    LR_LOADFROMFILE,
                )
                .ok()
                .filter(|h| !h.0.is_null())
                .map(|h| HICON(h.0))
            }
        } else {
            None
        };

        self.icon_cache.insert(icon_source.to_string(), loaded);
        loaded
    }
}

#[cfg(windows)]
impl Drop for GdiThemeCache {
    fn drop(&mut self) {
        unsafe {
            for (_, icon_opt) in self.icon_cache.drain() {
                if let Some(icon) = icon_opt {
                    let _ = DestroyIcon(icon);
                }
            }
            let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.border_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_hover_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_active_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_active_border_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.grip_dot_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.subtle_hint_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.warn_dot_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.icon_pen_normal.0));
            let _ = DeleteObject(HGDIOBJ(self.icon_pen_active.0));
        }
    }
}

pub struct PaintState<'a> {
    pub layout: &'a LauncherLayout,
    pub scene_state: &'a SceneState,
    pub last_known_scene_id: Option<&'a str>,
    pub active_in_overflow: bool,
    pub hovered: &'a HitTarget,
    pub has_warnings: bool,
}

#[cfg(windows)]
pub unsafe fn paint_launcher(hdc: HDC, theme: &mut GdiThemeCache, state: &PaintState<'_>) {
    let w = state.layout.width_px.max(1);
    let h = state.layout.height_px.max(1);
    let dpi = state.layout.dpi;
    theme.ensure_dpi(dpi);

    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem_dc, HGDIOBJ(mem_bmp.0));

    let full_rc = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };

    // 1. Dark background + 1px outer frame
    FillRect(mem_dc, &full_rc, theme.bg_brush);
    FrameRect(mem_dc, &full_rc, theme.border_brush);

    // 2. Draw dedicated grip dots (`⠿`)
    draw_grip_dots(mem_dc, theme, &state.layout.grip_rect, dpi);

    // 3. Draw each pinned scene button
    for (idx, slot) in state.layout.scene_buttons.iter().enumerate() {
        let is_active = matches!(state.scene_state, SceneState::Active(id) if id == &slot.scene.id);
        let is_last_hint = matches!(state.scene_state, SceneState::Unknown)
            && state.last_known_scene_id == Some(slot.scene.id.as_str());
        let is_hovered = matches!(
            state.hovered,
            HitTarget::SceneButton { index, .. } if *index == idx
        );

        draw_button_bg(mem_dc, theme, &slot.rect, is_active, is_hovered, is_last_hint, dpi);
        draw_scene_icon(
            mem_dc,
            theme,
            &slot.rect,
            &slot.scene.icon,
            &slot.scene.name,
            is_active,
            dpi,
        );
    }

    // 4. Draw optional `⋯` overflow button
    if let Some(ref ov_rect) = state.layout.overflow_button_rect {
        let is_active = state.active_in_overflow;
        let is_hovered = matches!(state.hovered, HitTarget::OverflowButton);
        draw_button_bg(mem_dc, theme, ov_rect, is_active, is_hovered, false, dpi);
        draw_overflow_dots(mem_dc, theme, ov_rect, is_active, dpi);
    }

    // 5. Non-blocking warning indicator dot in top-right corner if last switch had warnings (Section 2.1)
    if state.has_warnings {
        let r = dip_to_px(3, dpi).max(2);
        let pad = dip_to_px(3, dpi);
        let dot_rc = RECT {
            left: w - pad - r * 2,
            top: pad,
            right: w - pad,
            bottom: pad + r * 2,
        };
        FillRect(mem_dc, &dot_rc, theme.warn_dot_brush);
    }

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);

    let _ = SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(HGDIOBJ(mem_bmp.0));
    let _ = DeleteDC(mem_dc);
}

#[cfg(windows)]
unsafe fn draw_grip_dots(hdc: HDC, theme: &GdiThemeCache, rect: &RectData, dpi: u32) {
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let dot_size = dip_to_px(2, dpi).max(2);
    let step_x = dip_to_px(4, dpi).max(3);
    let step_y = dip_to_px(5, dpi).max(4);

    // 2 columns x 3 rows (`⠿`)
    for col in [-1i32, 1i32] {
        for row in [-1i32, 0i32, 1i32] {
            let x = cx + (col * step_x) / 2 - dot_size / 2;
            let y = cy + row * step_y - dot_size / 2;
            let rc = RECT {
                left: x,
                top: y,
                right: x + dot_size,
                bottom: y + dot_size,
            };
            FillRect(hdc, &rc, theme.grip_dot_brush);
        }
    }
}

#[cfg(windows)]
unsafe fn draw_button_bg(
    hdc: HDC,
    theme: &GdiThemeCache,
    rect: &RectData,
    is_active: bool,
    is_hovered: bool,
    is_last_hint: bool,
    dpi: u32,
) {
    let rc = RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };

    if is_active {
        FillRect(hdc, &rc, theme.btn_active_brush);
        FrameRect(hdc, &rc, theme.btn_active_border_brush);
    } else if is_hovered {
        FillRect(hdc, &rc, theme.btn_hover_brush);
        FrameRect(hdc, &rc, theme.border_brush);
    }

    // Subtle bottom bar when state is Unknown and this was the previous scene (Section 2.1 & 12)
    if !is_active && is_last_hint {
        let bar_h = dip_to_px(2, dpi).max(1);
        let inset = dip_to_px(8, dpi);
        let hint_rc = RECT {
            left: rect.left + inset,
            top: rect.bottom - bar_h - 1,
            right: rect.right - inset,
            bottom: rect.bottom - 1,
        };
        FillRect(hdc, &hint_rc, theme.subtle_hint_brush);
    }
}

#[cfg(windows)]
pub unsafe fn draw_scene_icon(
    hdc: HDC,
    theme: &mut GdiThemeCache,
    rect: &RectData,
    icon_name: &str,
    scene_name: &str,
    is_active: bool,
    dpi: u32,
) {
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let half = dip_to_px(8, dpi).max(6);

    // Check if an external .ico is configured and loadable
    if let Some(hicon) = theme.get_or_load_icon(icon_name, dpi) {
        let size = dip_to_px(18, dpi);
        let _ = DrawIconEx(
            hdc,
            cx - size / 2,
            cy - size / 2,
            hicon,
            size,
            size,
            0,
            None,
            DI_NORMAL,
        );
        return;
    }

    let pen = if is_active {
        theme.icon_pen_active
    } else {
        theme.icon_pen_normal
    };
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(hdc, HGDIOBJ(theme.bg_brush.0));
    let _ = SetBkMode(hdc, TRANSPARENT);

    match icon_name {
        "chart" => {
            // Rising candlestick / trend line icon for Trade
            let x1 = cx - half;
            let x2 = cx - half / 3;
            let x3 = cx + half / 3;
            let x4 = cx + half;

            let _ = MoveToEx(hdc, x1, cy + half / 2, None);
            let _ = LineTo(hdc, x2, cy - half / 4);
            let _ = LineTo(hdc, x3, cy + half / 4);
            let _ = LineTo(hdc, x4, cy - half / 2);

            // Arrowhead at top-right
            let arrow = dip_to_px(4, dpi).max(3);
            let _ = MoveToEx(hdc, x4 - arrow, cy - half / 2, None);
            let _ = LineTo(hdc, x4, cy - half / 2);
            let _ = LineTo(hdc, x4, cy - half / 2 + arrow);
        }
        "code" => {
            // `< / >` brackets for Code
            let w_brk = dip_to_px(4, dpi).max(3);
            let h_brk = dip_to_px(5, dpi).max(4);
            // Left `<`
            let _ = MoveToEx(hdc, cx - w_brk, cy - h_brk, None);
            let _ = LineTo(hdc, cx - half, cy);
            let _ = LineTo(hdc, cx - w_brk, cy + h_brk);
            // Right `>`
            let _ = MoveToEx(hdc, cx + w_brk, cy - h_brk, None);
            let _ = LineTo(hdc, cx + half, cy);
            let _ = LineTo(hdc, cx + w_brk, cy + h_brk);
            // Center `/`
            let slash_x = dip_to_px(2, dpi).max(1);
            let _ = MoveToEx(hdc, cx + slash_x, cy - h_brk - 1, None);
            let _ = LineTo(hdc, cx - slash_x, cy + h_brk + 1);
        }
        "globe" => {
            // Circle + horizontal equator + vertical meridian for Web
            let _ = Ellipse(hdc, cx - half, cy - half, cx + half, cy + half);
            let _ = MoveToEx(hdc, cx - half, cy, None);
            let _ = LineTo(hdc, cx + half, cy);
            let _ = MoveToEx(hdc, cx, cy - half, None);
            let _ = LineTo(hdc, cx, cy + half);
        }
        _ => {
            // Rounded square monogram fallback
            let r = dip_to_px(4, dpi).max(2);
            let _ = RoundRect(hdc, cx - half, cy - half, cx + half, cy + half, r, r);
            let _ = SetTextColor(
                hdc,
                if is_active {
                    rgb(255, 255, 255)
                } else {
                    rgb(205, 212, 224)
                },
            );
            let _ = scene_name;
        }
    }

    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old_pen);
}

#[cfg(windows)]
unsafe fn draw_overflow_dots(
    hdc: HDC,
    theme: &GdiThemeCache,
    rect: &RectData,
    is_active: bool,
    dpi: u32,
) {
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let dot_r = dip_to_px(2, dpi).max(1);
    let spacing = dip_to_px(6, dpi).max(4);
    let brush = if is_active {
        theme.btn_active_border_brush
    } else {
        theme.grip_dot_brush
    };

    for offset in [-1i32, 0i32, 1i32] {
        let x = cx + offset * spacing;
        let rc = RECT {
            left: x - dot_r,
            top: cy - dot_r,
            right: x + dot_r + 1,
            bottom: cy + dot_r + 1,
        };
        FillRect(hdc, &rc, brush);
    }
}
