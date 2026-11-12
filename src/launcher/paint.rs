//! Double-buffered GDI rendering and high-quality GDI+ antialiased icons (`paint.rs`).
//!
//! Per Section 2.1 & 7.3 of spec.md:
//! - Reuses cached `HBRUSH` objects via `GdiThemeCache` and releases them in `Drop`.
//! - Uses off-screen memory DC (`CreateCompatibleDC` + `CreateCompatibleBitmap`) to eliminate flicker.
//! - Uses GDI+ with `SmoothingModeAntiAlias` and `InterpolationModeHighQualityBicubic`
//!   to draw crisp, antialiased DPI-scaled vector icons (`"chart"`, `"code"`, `"globe"`),
//!   overflow dots (`"⋯"`), grip dots (`⠿`), and external icons/images without pixelation or jaggies on high-DPI (2K/4K) monitors.
//! - Clearly distinguishes `Active(scene)` (accent highlight + border) vs `Unknown` (unselected,
//!   with an optional subtle underline dot for `last_known_scene_id`).

use crate::config::RectData;
use crate::launcher::input::{HitTarget, LauncherLayout};
use crate::platform::monitors::dip_to_px;
use crate::scenes::manager::SceneState;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, FrameRect, SelectObject, HBRUSH, HDC, HGDIOBJ, SRCCOPY,
};
#[cfg(windows)]
use windows::Win32::Graphics::GdiPlus::{
    DashCapRound, GdipCreateBitmapFromHICON, GdipCreateFromHDC, GdipCreatePen1,
    GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePen, GdipDisposeImage,
    GdipDrawEllipse, GdipDrawImageRect, GdipDrawLine, GdipFillEllipse, GdipLoadImageFromFile,
    GdipSetInterpolationMode, GdipSetPenLineCap197819, GdipSetPenLineJoin, GdipSetSmoothingMode,
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpBrush, GpGraphics, GpImage,
    GpPen, GpSolidFill, InterpolationModeHighQualityBicubic, LineCapRound, LineJoinRound,
    Ok as GpOk, SmoothingModeAntiAlias, UnitPixel,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, LoadImageW, HICON, IMAGE_ICON, LR_LOADFROMFILE,
};

#[cfg(windows)]
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

#[cfg(windows)]
struct GdiplusToken(usize);

#[cfg(windows)]
impl Drop for GdiplusToken {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                GdiplusShutdown(self.0);
            }
        }
    }
}

/// RAII wrapper for GDI+ library lifecycle.
#[cfg(windows)]
#[derive(Clone)]
pub struct GdiplusContext {
    _token: Arc<GdiplusToken>,
}

#[cfg(windows)]
impl GdiplusContext {
    pub fn init() -> Self {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: false.into(),
            SuppressExternalCodecs: false.into(),
        };
        let mut token = 0usize;
        unsafe {
            let _ = GdiplusStartup(&mut token, &input, std::ptr::null_mut());
        }
        Self {
            _token: Arc::new(GdiplusToken(token)),
        }
    }
}

/// Cached GDI brushes and GDI+ resources so we never leak objects per `WM_PAINT` (Section 7.3).
#[cfg(windows)]
pub struct GdiThemeCache {
    pub gdiplus: GdiplusContext,
    pub bg_brush: HBRUSH,
    pub border_brush: HBRUSH,
    pub btn_hover_brush: HBRUSH,
    pub btn_active_brush: HBRUSH,
    pub btn_active_border_brush: HBRUSH,
    pub subtle_hint_brush: HBRUSH,
    pub cached_dpi: u32,
    pub image_cache: HashMap<String, Option<*mut GpImage>>,
}

#[cfg(windows)]
impl GdiThemeCache {
    pub fn new(dpi: u32) -> Self {
        Self::with_context(dpi, GdiplusContext::init())
    }

    pub fn with_context(dpi: u32, gdiplus: GdiplusContext) -> Self {
        unsafe {
            Self {
                gdiplus,
                bg_brush: CreateSolidBrush(rgb(24, 26, 31)),
                border_brush: CreateSolidBrush(rgb(52, 57, 68)),
                btn_hover_brush: CreateSolidBrush(rgb(42, 46, 56)),
                btn_active_brush: CreateSolidBrush(rgb(33, 76, 138)),
                btn_active_border_brush: CreateSolidBrush(rgb(76, 148, 246)),
                subtle_hint_brush: CreateSolidBrush(rgb(90, 115, 155)),
                cached_dpi: dpi,
                image_cache: HashMap::new(),
            }
        }
    }

    pub fn ensure_dpi(&mut self, dpi: u32) {
        if self.cached_dpi != dpi {
            unsafe {
                for (_, img_opt) in self.image_cache.drain() {
                    if let Some(img) = img_opt {
                        let _ = GdipDisposeImage(img);
                    }
                }
            }
            let ctx = self.gdiplus.clone();
            *self = Self::with_context(dpi, ctx);
        }
    }

    /// Load or retrieve a cached GDI+ image for external icons/images (`.ico`, `.png`, etc.).
    /// For `.ico` files, leverages Windows icon loader to pick the nearest DPI resolution frame,
    /// then wraps it into GDI+ for smooth bicubic scaling.
    pub fn get_or_load_image(&mut self, icon_source: &str, dpi: u32) -> Option<*mut GpImage> {
        // Built-in vector icons never touch the filesystem
        if icon_source.is_empty() || matches!(icon_source, "chart" | "code" | "globe") {
            return None;
        }
        if let Some(&cached) = self.image_cache.get(icon_source) {
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
            let target_size = dip_to_px(18, dpi);

            let mut gp_image: Option<*mut GpImage> = None;
            unsafe {
                // 1. Try loading via LoadImageW (ideal for .ico to pick the right resolution icon entry)
                if let Ok(hicon) = LoadImageW(
                    None,
                    windows::core::PCWSTR(wide.as_ptr()),
                    IMAGE_ICON,
                    target_size,
                    target_size,
                    LR_LOADFROMFILE,
                ) {
                    if !hicon.0.is_null() {
                        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
                        if GdipCreateBitmapFromHICON(HICON(hicon.0), &mut bitmap) == GpOk
                            && !bitmap.is_null()
                        {
                            gp_image = Some(bitmap as *mut GpImage);
                        }
                        let _ = DestroyIcon(HICON(hicon.0));
                    }
                }

                // 2. If LoadImageW didn't load (e.g. .png / .jpg), load directly via GDI+
                if gp_image.is_none() {
                    let mut img: *mut GpImage = std::ptr::null_mut();
                    if GdipLoadImageFromFile(windows::core::PCWSTR(wide.as_ptr()), &mut img) == GpOk
                        && !img.is_null()
                    {
                        gp_image = Some(img);
                    }
                }
            }
            gp_image
        } else {
            None
        };

        self.image_cache.insert(icon_source.to_string(), loaded);
        loaded
    }
}

#[cfg(windows)]
impl Drop for GdiThemeCache {
    fn drop(&mut self) {
        unsafe {
            for (_, img_opt) in self.image_cache.drain() {
                if let Some(img) = img_opt {
                    let _ = GdipDisposeImage(img);
                }
            }
            let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.border_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_hover_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_active_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.btn_active_border_brush.0));
            let _ = DeleteObject(HGDIOBJ(self.subtle_hint_brush.0));
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

    // 2. Button backgrounds
    for (idx, slot) in state.layout.scene_buttons.iter().enumerate() {
        let is_active = matches!(state.scene_state, SceneState::Active(id) if id == &slot.scene.id);
        let is_last_hint = matches!(state.scene_state, SceneState::Unknown)
            && state.last_known_scene_id == Some(slot.scene.id.as_str());
        let is_hovered = matches!(
            state.hovered,
            HitTarget::SceneButton { index, .. } if *index == idx
        );

        draw_button_bg(mem_dc, theme, &slot.rect, is_active, is_hovered, is_last_hint, dpi);
    }

    if let Some(ref ov_rect) = state.layout.overflow_button_rect {
        let is_active = state.active_in_overflow;
        let is_hovered = matches!(state.hovered, HitTarget::OverflowButton);
        draw_button_bg(mem_dc, theme, ov_rect, is_active, is_hovered, false, dpi);
    }

    // 3. Render antialiased vector icons, grip dots, overflow dots, and warning indicator using GDI+
    let mut graphics: *mut GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(mem_dc, &mut graphics) == GpOk && !graphics.is_null() {
        let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);

        // Grip dots (`⠿`)
        draw_grip_dots_gdiplus(graphics, &state.layout.grip_rect, dpi);

        // Scene icons
        for slot in &state.layout.scene_buttons {
            let is_active = matches!(state.scene_state, SceneState::Active(id) if id == &slot.scene.id);
            draw_scene_icon(
                graphics,
                theme,
                &slot.rect,
                &slot.scene.icon,
                &slot.scene.name,
                is_active,
                dpi,
            );
        }

        // Overflow dots (`⋯`)
        if let Some(ref ov_rect) = state.layout.overflow_button_rect {
            let is_active = state.active_in_overflow;
            draw_overflow_dots_gdiplus(graphics, ov_rect, is_active, dpi);
        }

        // Warning indicator dot
        if state.has_warnings {
            draw_warn_dot_gdiplus(graphics, w, dpi);
        }

        let _ = GdipDeleteGraphics(graphics);
    }

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);

    let _ = SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(HGDIOBJ(mem_bmp.0));
    let _ = DeleteDC(mem_dc);
}

#[cfg(windows)]
unsafe fn draw_grip_dots_gdiplus(graphics: *mut GpGraphics, rect: &RectData, dpi: u32) {
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let dot_r = (1.3 * dpi as f32 / 96.0).max(1.0);
    let step_x = (4.0 * dpi as f32 / 96.0).max(3.0);
    let step_y = (5.0 * dpi as f32 / 96.0).max(4.0);

    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(0xFF6E7687, &mut brush) == GpOk {
        for col in [-1.0f32, 1.0f32] {
            for row in [-1.0f32, 0.0f32, 1.0f32] {
                let x = cx + (col * step_x) / 2.0;
                let y = cy + row * step_y;
                let _ = GdipFillEllipse(
                    graphics,
                    brush as *mut GpBrush,
                    x - dot_r,
                    y - dot_r,
                    dot_r * 2.0,
                    dot_r * 2.0,
                );
            }
        }
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
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
    graphics: *mut GpGraphics,
    theme: &mut GdiThemeCache,
    rect: &RectData,
    icon_name: &str,
    scene_name: &str,
    is_active: bool,
    dpi: u32,
) {
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;

    // Check if an external image/icon file is configured and loadable
    if let Some(img) = theme.get_or_load_image(icon_name, dpi) {
        let size = (18.0 * dpi as f32 / 96.0).max(12.0);
        let _ = GdipDrawImageRect(
            graphics,
            img,
            cx - size / 2.0,
            cy - size / 2.0,
            size,
            size,
        );
        return;
    }

    let half = (8.0 * dpi as f32 / 96.0).max(6.0);
    let stroke_w = (1.8 * dpi as f32 / 96.0).max(1.5);
    let color = if is_active {
        0xFFFFFFFF // Crisp pure white
    } else {
        0xFFCDD4E0 // rgb(205, 212, 224)
    };

    let mut pen: *mut GpPen = std::ptr::null_mut();
    if GdipCreatePen1(color, stroke_w, UnitPixel, &mut pen) != GpOk {
        return;
    }
    let _ = GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapRound);
    let _ = GdipSetPenLineJoin(pen, LineJoinRound);

    match icon_name {
        "chart" => {
            // Rising candlestick / trend line icon for Trade
            let x1 = cx - half;
            let y1 = cy + half * 0.45;
            let x2 = cx - half / 3.0;
            let y2 = cy - half * 0.22;
            let x3 = cx + half / 3.0;
            let y3 = cy + half * 0.22;
            let x4 = cx + half;
            let y4 = cy - half * 0.45;

            let _ = GdipDrawLine(graphics, pen, x1, y1, x2, y2);
            let _ = GdipDrawLine(graphics, pen, x2, y2, x3, y3);
            let _ = GdipDrawLine(graphics, pen, x3, y3, x4, y4);

            // Arrowhead at top-right
            let arrow = (3.5 * dpi as f32 / 96.0).max(2.5);
            let _ = GdipDrawLine(graphics, pen, x4 - arrow, y4, x4, y4);
            let _ = GdipDrawLine(graphics, pen, x4, y4, x4, y4 + arrow);
        }
        "code" => {
            // `< / >` brackets for Code
            let w_brk = (3.5 * dpi as f32 / 96.0).max(2.5);
            let h_brk = (5.0 * dpi as f32 / 96.0).max(4.0);

            // Left `<`
            let _ = GdipDrawLine(graphics, pen, cx - w_brk, cy - h_brk, cx - half, cy);
            let _ = GdipDrawLine(graphics, pen, cx - half, cy, cx - w_brk, cy + h_brk);

            // Right `>`
            let _ = GdipDrawLine(graphics, pen, cx + w_brk, cy - h_brk, cx + half, cy);
            let _ = GdipDrawLine(graphics, pen, cx + half, cy, cx + w_brk, cy + h_brk);

            // Center `/`
            let slash_x = (1.8 * dpi as f32 / 96.0).max(1.2);
            let _ = GdipDrawLine(
                graphics,
                pen,
                cx + slash_x,
                cy - h_brk - 1.0,
                cx - slash_x,
                cy + h_brk + 1.0,
            );
        }
        "globe" => {
            // Circle + horizontal equator + vertical meridian for Web
            let _ = GdipDrawEllipse(graphics, pen, cx - half, cy - half, half * 2.0, half * 2.0);
            let _ = GdipDrawLine(graphics, pen, cx - half, cy, cx + half, cy);
            let v_half = half * 0.45;
            let _ = GdipDrawEllipse(graphics, pen, cx - v_half, cy - half, v_half * 2.0, half * 2.0);
        }
        _ => {
            // Fallback outline circle
            let _ = GdipDrawEllipse(graphics, pen, cx - half, cy - half, half * 2.0, half * 2.0);
            let _ = scene_name;
        }
    }

    let _ = GdipDeletePen(pen);
}

#[cfg(windows)]
unsafe fn draw_overflow_dots_gdiplus(
    graphics: *mut GpGraphics,
    rect: &RectData,
    is_active: bool,
    dpi: u32,
) {
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let dot_r = (1.5 * dpi as f32 / 96.0).max(1.2);
    let spacing = (6.0 * dpi as f32 / 96.0).max(4.5);

    let color = if is_active {
        0xFF4C94F6 // rgb(76, 148, 246)
    } else {
        0xFF6E7687 // rgb(110, 118, 135)
    };

    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(color, &mut brush) == GpOk {
        for offset in [-1.0f32, 0.0f32, 1.0f32] {
            let x = cx + offset * spacing;
            let _ = GdipFillEllipse(
                graphics,
                brush as *mut GpBrush,
                x - dot_r,
                cy - dot_r,
                dot_r * 2.0,
                dot_r * 2.0,
            );
        }
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
    }
}

#[cfg(windows)]
unsafe fn draw_warn_dot_gdiplus(graphics: *mut GpGraphics, w: i32, dpi: u32) {
    let r = (2.5 * dpi as f32 / 96.0).max(2.0);
    let pad = (4.0 * dpi as f32 / 96.0).max(3.0);
    let cx = w as f32 - pad - r;
    let cy = pad + r;

    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(0xFFF0A028, &mut brush) == GpOk {
        let _ = GdipFillEllipse(
            graphics,
            brush as *mut GpBrush,
            cx - r,
            cy - r,
            r * 2.0,
            r * 2.0,
        );
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
    }
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn test_gdiplus_context_and_theme_cache() {
        let mut theme = GdiThemeCache::new(96);
        assert_eq!(theme.cached_dpi, 96);

        // Test ensure_dpi updates without error
        theme.ensure_dpi(120);
        assert_eq!(theme.cached_dpi, 120);
        theme.ensure_dpi(144);
        assert_eq!(theme.cached_dpi, 144);
    }

    #[test]
    fn test_gdiplus_icon_rendering_all_dpis() {
        use windows::Win32::Graphics::Gdi::*;
        unsafe {
            let hdc = CreateCompatibleDC(None);
            let mut theme = GdiThemeCache::new(96);

            for &dpi in &[96u32, 120, 144, 192] {
                theme.ensure_dpi(dpi);
                let w = dip_to_px(32, dpi);
                let h = dip_to_px(32, dpi);
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let old_bmp = SelectObject(hdc, HGDIOBJ(bmp.0));

                let mut graphics: *mut GpGraphics = std::ptr::null_mut();
                assert_eq!(GdipCreateFromHDC(hdc, &mut graphics), GpOk);
                assert_eq!(GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias), GpOk);

                let rect = RectData {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };

                for icon in &["chart", "code", "globe", "custom_fallback"] {
                    draw_scene_icon(graphics, &mut theme, &rect, icon, "Test", true, dpi);
                    draw_scene_icon(graphics, &mut theme, &rect, icon, "Test", false, dpi);
                }

                draw_grip_dots_gdiplus(graphics, &rect, dpi);
                draw_overflow_dots_gdiplus(graphics, &rect, true, dpi);
                draw_overflow_dots_gdiplus(graphics, &rect, false, dpi);
                draw_warn_dot_gdiplus(graphics, w, dpi);

                let _ = GdipDeleteGraphics(graphics);
                let _ = SelectObject(hdc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
            }

            let _ = DeleteDC(hdc);
        }
    }
}


