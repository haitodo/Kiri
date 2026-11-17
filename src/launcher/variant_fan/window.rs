//! Layered `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT` fan overlay rendered via GDI+ + `UpdateLayeredWindow`.

use super::geometry::{
    FanLayout, PILL_DOT_CX_DIP, PILL_DOT_GAP_DIP, PILL_DOT_RADIUS_DIP, PILL_TEXT_RIGHT_PAD_DIP,
};
use crate::config::Scene;
use crate::launcher::paint::{
    colors, draw_rounded_rect_border_gdiplus, fill_rounded_rect_gdiplus, GdiplusContext,
};
use crate::platform::monitors::dip_to_px;
use std::f32::consts::PI;
use std::sync::Once;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP,
    HGDIOBJ,
};
use windows::Win32::Graphics::GdiPlus::{
    DashCapRound, FontStyleBold, FontStyleRegular, GdipCreateFont, GdipCreateFontFamilyFromName,
    GdipCreateFromHDC, GdipCreatePen1, GdipCreateSolidFill, GdipCreateStringFormat,
    GdipDeleteBrush, GdipDeleteFont, GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePen,
    GdipDeleteStringFormat, GdipDrawArc, GdipDrawEllipse, GdipDrawLine, GdipDrawString,
    GdipFillEllipse, GdipMeasureString, GdipSetPenLineCap197819, GdipSetSmoothingMode,
    GdipSetStringFormatAlign, GdipSetStringFormatFlags, GdipSetStringFormatLineAlign,
    GdipSetStringFormatTrimming, GdipSetTextRenderingHint, GpBrush, GpFont, GpFontFamily,
    GpGraphics, GpPen, GpSolidFill, GpStringFormat, LineCapRound, Ok as GpOk, RectF,
    SmoothingModeAntiAlias, StringAlignmentCenter, StringAlignmentNear, StringFormatFlagsNoWrap,
    StringTrimmingEllipsisCharacter, TextRenderingHintAntiAliasGridFit, UnitPixel,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

struct FontResources {
    dpi: u32,
    family: *mut GpFontFamily,
    font_regular: *mut GpFont,
    font_bold: *mut GpFont,
    format_left: *mut GpStringFormat,
    format_measure: *mut GpStringFormat,
}

impl FontResources {
    unsafe fn new(dpi: u32) -> Option<Self> {
        let mut family: *mut GpFontFamily = std::ptr::null_mut();
        if GdipCreateFontFamilyFromName(w!("Segoe UI Variable Text"), std::ptr::null_mut(), &mut family)
            != GpOk
            || family.is_null()
        {
            if GdipCreateFontFamilyFromName(w!("Segoe UI"), std::ptr::null_mut(), &mut family)
                != GpOk
                || family.is_null()
            {
                return None;
            }
        }
        let px_size = (12.5 * dpi.max(96) as f32 / 96.0).max(11.0);
        let mut font_regular: *mut GpFont = std::ptr::null_mut();
        let mut font_bold: *mut GpFont = std::ptr::null_mut();
        let _ = GdipCreateFont(
            family,
            px_size,
            FontStyleRegular.0,
            UnitPixel,
            &mut font_regular,
        );
        let _ = GdipCreateFont(
            family,
            px_size,
            FontStyleBold.0,
            UnitPixel,
            &mut font_bold,
        );
        if font_regular.is_null() {
            let _ = GdipDeleteFontFamily(family);
            return None;
        }
        if font_bold.is_null() {
            font_bold = font_regular;
        }

        let mut format_left: *mut GpStringFormat = std::ptr::null_mut();
        let _ = GdipCreateStringFormat(0, 0, &mut format_left);
        if !format_left.is_null() {
            let _ = GdipSetStringFormatFlags(format_left, StringFormatFlagsNoWrap.0);
            let _ = GdipSetStringFormatAlign(format_left, StringAlignmentNear);
            let _ = GdipSetStringFormatLineAlign(format_left, StringAlignmentCenter);
            let _ = GdipSetStringFormatTrimming(format_left, StringTrimmingEllipsisCharacter);
        }

        let mut format_measure: *mut GpStringFormat = std::ptr::null_mut();
        let _ = GdipCreateStringFormat(0, 0, &mut format_measure);
        if !format_measure.is_null() {
            let _ = GdipSetStringFormatFlags(format_measure, StringFormatFlagsNoWrap.0);
            let _ = GdipSetStringFormatAlign(format_measure, StringAlignmentNear);
            let _ = GdipSetStringFormatLineAlign(format_measure, StringAlignmentCenter);
        }

        Some(Self {
            dpi,
            family,
            font_regular,
            font_bold,
            format_left,
            format_measure,
        })
    }
}

impl Drop for FontResources {
    fn drop(&mut self) {
        unsafe {
            if !self.format_left.is_null() {
                let _ = GdipDeleteStringFormat(self.format_left);
            }
            if !self.format_measure.is_null() {
                let _ = GdipDeleteStringFormat(self.format_measure);
            }
            if !self.font_bold.is_null() && self.font_bold != self.font_regular {
                let _ = GdipDeleteFont(self.font_bold);
            }
            if !self.font_regular.is_null() {
                let _ = GdipDeleteFont(self.font_regular);
            }
            if !self.family.is_null() {
                let _ = GdipDeleteFontFamily(self.family);
            }
        }
    }
}

pub struct VariantFanWindow {
    hwnd: HWND,
    fonts: Option<FontResources>,
    visible: bool,
    // Keep GDI+ context last so it drops after `FontResources`.
    _gdiplus: GdiplusContext,
}

impl VariantFanWindow {
    pub fn create(gdiplus: GdiplusContext) -> Result<Self, String> {
        static REGISTER_ONCE: Once = Once::new();
        let class = w!("KiriVariantFanOverlayClass");

        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            REGISTER_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(fan_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: class,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW,
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .map_err(|e| format!("Failed to create variant fan overlay: {e}"))?;

            Ok(Self {
                hwnd,
                _gdiplus: gdiplus,
                fonts: FontResources::new(96),
                visible: false,
            })
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    fn ensure_fonts(&mut self, dpi: u32) {
        let dpi = dpi.max(96);
        if self.fonts.as_ref().is_none_or(|f| f.dpi != dpi) {
            self.fonts = unsafe { FontResources::new(dpi) };
        }
    }

    /// Measure label widths in physical pixels at `dpi` so `FanLayout` sizes pills accurately.
    pub fn measure_label_widths(&mut self, scene: &Scene, dpi: u32) -> Vec<i32> {
        self.ensure_fonts(dpi);
        let fallback_per_char = dip_to_px(10, dpi);
        let Some(ref fonts) = self.fonts else {
            return scene
                .variants
                .iter()
                .map(|v| (v.name.chars().count() as i32) * fallback_per_char)
                .collect();
        };
        unsafe {
            let mem_dc = CreateCompatibleDC(None);
            let mut graphics: *mut GpGraphics = std::ptr::null_mut();
            let ok = !mem_dc.is_invalid()
                && GdipCreateFromHDC(mem_dc, &mut graphics) == GpOk
                && !graphics.is_null();
            if ok {
                // Match the rendering hints used in `render` so glyph metrics align accurately.
                let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
                let _ = GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAliasGridFit);
            }
            let widths = scene
                .variants
                .iter()
                .map(|v| {
                    if !ok {
                        return (v.name.chars().count() as i32) * fallback_per_char;
                    }
                    let wide: Vec<u16> = v.name.encode_utf16().collect();
                    if wide.is_empty() {
                        return dip_to_px(24, dpi);
                    }
                    let layout_rect = RectF {
                        X: 0.0,
                        Y: 0.0,
                        Width: 4096.0,
                        Height: 256.0,
                    };
                    let mut bound_bold = RectF::default();
                    let status_bold = GdipMeasureString(
                        graphics,
                        windows::core::PCWSTR(wide.as_ptr()),
                        wide.len() as i32,
                        fonts.font_bold,
                        &layout_rect,
                        fonts.format_measure,
                        &mut bound_bold,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                    let mut bound_reg = RectF::default();
                    let status_reg = GdipMeasureString(
                        graphics,
                        windows::core::PCWSTR(wide.as_ptr()),
                        wide.len() as i32,
                        fonts.font_regular,
                        &layout_rect,
                        fonts.format_measure,
                        &mut bound_reg,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                    match (status_bold == GpOk, status_reg == GpOk) {
                        (true, true) => bound_bold.Width.max(bound_reg.Width).ceil() as i32,
                        (true, false) => bound_bold.Width.ceil() as i32,
                        (false, true) => bound_reg.Width.ceil() as i32,
                        (false, false) => (v.name.chars().count() as i32) * fallback_per_char,
                    }
                })
                .collect();
            if !graphics.is_null() {
                let _ = GdipDeleteGraphics(graphics);
            }
            if !mem_dc.is_invalid() {
                let _ = DeleteDC(mem_dc);
            }
            widths
        }
    }

    pub fn render(
        &mut self,
        scene: &Scene,
        layout: &FanLayout,
        selected_index: Option<usize>,
        preferred_index: Option<usize>,
        dpi: u32,
    ) {
        let dpi = dpi.max(96);
        self.ensure_fonts(dpi);
        let w = layout.bounds.width().max(1);
        let h = layout.bounds.height().max(1);

        unsafe {
            let mem_dc = CreateCompatibleDC(None);
            if mem_dc.is_invalid() {
                return;
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let dib: HBITMAP = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bmp) if !bmp.is_invalid() => bmp,
                _ => {
                    let _ = DeleteDC(mem_dc);
                    return;
                }
            };
            let old_bmp = SelectObject(mem_dc, HGDIOBJ(dib.0));

            if !bits.is_null() {
                std::ptr::write_bytes(bits as *mut u8, 0, (w as usize) * (h as usize) * 4);
            }

            let mut graphics: *mut GpGraphics = std::ptr::null_mut();
            if GdipCreateFromHDC(mem_dc, &mut graphics) == GpOk && !graphics.is_null() {
                let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
                let _ = GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAliasGridFit);
                self.paint_scene_fan(graphics, scene, layout, selected_index, preferred_index, dpi);
                let _ = GdipDeleteGraphics(graphics);
            }

            let dst_pt = POINT {
                x: layout.bounds.left,
                y: layout.bounds.top,
            };
            let size = SIZE { cx: w, cy: h };
            let src_pt = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                layout.bounds.left,
                layout.bounds.top,
                w,
                h,
                SWP_NOACTIVATE,
            );
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&dst_pt),
                Some(&size),
                Some(mem_dc),
                Some(&src_pt),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if !self.visible {
                self.visible = true;
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }

            let _ = SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(mem_dc);
        }
    }

    unsafe fn paint_scene_fan(
        &self,
        graphics: *mut GpGraphics,
        scene: &Scene,
        layout: &FanLayout,
        selected_index: Option<usize>,
        preferred_index: Option<usize>,
        dpi: u32,
    ) {
        let scale = dpi as f32 / 96.0;
        let ox = (layout.origin_x - layout.bounds.left) as f32;
        let oy = (layout.origin_y - layout.bounds.top) as f32;
        let dz = layout.deadzone_radius as f32;

        // 1. Subtle deadzone ring at origin so user knows returning to center cancels
        let ring_color = if selected_index.is_none() {
            0xB84C94F6
        } else {
            0x668C96A8
        };
        draw_circle_outline(graphics, ox, oy, dz, ring_color, (1.5 * scale).max(1.2));

        // 2. Subtle arc connecting pills in radial mode
        if !layout.is_list_fallback && layout.items.len() >= 2 {
            let mut min_deg = f32::INFINITY;
            let mut max_deg = f32::NEG_INFINITY;
            let mut avg_r = 0.0f32;
            for item in &layout.items {
                let dx = (item.center_x - layout.origin_x) as f32;
                let dy = (item.center_y - layout.origin_y) as f32;
                avg_r += (dx * dx + dy * dy).sqrt();
                let mut deg = dy.atan2(dx) * 180.0 / PI;
                // Unwrap relative to base_angle
                let base_deg = (layout.base_angle as f32) * 180.0 / PI;
                while deg - base_deg > 180.0 {
                    deg -= 360.0;
                }
                while deg - base_deg < -180.0 {
                    deg += 360.0;
                }
                min_deg = min_deg.min(deg);
                max_deg = max_deg.max(deg);
            }
            avg_r /= layout.items.len() as f32;
            if avg_r > dz && (max_deg - min_deg).abs() > 1.0 {
                let mut arc_pen: *mut GpPen = std::ptr::null_mut();
                if GdipCreatePen1(0x558C96A8, (1.5 * scale).max(1.0), UnitPixel, &mut arc_pen) == GpOk
                    && !arc_pen.is_null()
                {
                    let _ = GdipSetPenLineCap197819(arc_pen, LineCapRound, LineCapRound, DashCapRound);
                    let _ = GdipDrawArc(
                        graphics,
                        arc_pen,
                        ox - avg_r,
                        oy - avg_r,
                        avg_r * 2.0,
                        avg_r * 2.0,
                        min_deg,
                        max_deg - min_deg,
                    );
                    let _ = GdipDeletePen(arc_pen);
                }
            }
        }

        // 3. Highlight spoke from origin deadzone edge to selected pill
        if let Some(sel) = selected_index.and_then(|i| layout.items.get(i)) {
            let tx = (sel.center_x - layout.bounds.left) as f32;
            let ty = (sel.center_y - layout.bounds.top) as f32;
            let dx = tx - ox;
            let dy = ty - oy;
            let dist = (dx * dx + dy * dy).sqrt().max(1.0);
            let sx = ox + dx / dist * dz;
            let sy = oy + dy / dist * dz;
            let mut spoke_pen: *mut GpPen = std::ptr::null_mut();
            if GdipCreatePen1(0xDD4C94F6, (2.2 * scale).max(1.5), UnitPixel, &mut spoke_pen) == GpOk
                && !spoke_pen.is_null()
            {
                let _ = GdipSetPenLineCap197819(spoke_pen, LineCapRound, LineCapRound, DashCapRound);
                let _ = GdipDrawLine(graphics, spoke_pen, sx, sy, tx, ty);
                let _ = GdipDeletePen(spoke_pen);
            }
        }

        // 4. Variant pills
        let radius = (8.0 * scale).max(5.0);
        let stroke = (1.2 * scale).max(1.0);
        let shadow_off = (2.0 * scale).max(1.5);
        let dot_r = (PILL_DOT_RADIUS_DIP as f32 * scale).max(2.2);

        for item in &layout.items {
            let Some(variant) = scene.variants.get(item.index) else {
                continue;
            };
            let x = (item.rect.left - layout.bounds.left) as f32;
            let y = (item.rect.top - layout.bounds.top) as f32;
            let pw = item.rect.width() as f32;
            let ph = item.rect.height() as f32;

            let is_selected = selected_index == Some(item.index);
            let is_preferred = preferred_index == Some(item.index);

            // Drop shadow
            fill_rounded_rect_gdiplus(
                graphics,
                x,
                y + shadow_off,
                pw,
                ph,
                radius,
                0x48000000,
            );

            let (fill_argb, border_argb, text_argb) = if is_selected {
                (0xF5214C8A, colors::BTN_ACTIVE_BORDER_ARGB, 0xFFFFFFFF)
            } else if is_preferred {
                (0xEB1D222C, 0xCC4C94F6, colors::TEXT_PRIMARY_ARGB)
            } else {
                (0xEB181A1F, 0xE63C424F, colors::TEXT_PRIMARY_ARGB)
            };

            fill_rounded_rect_gdiplus(graphics, x, y, pw, ph, radius, fill_argb);
            draw_rounded_rect_border_gdiplus(
                graphics,
                x,
                y,
                pw,
                ph,
                radius,
                border_argb,
                if is_selected { stroke * 1.4 } else { stroke },
            );

            // Preferred indicator dot on left
            let dot_cx = x + (PILL_DOT_CX_DIP as f32 * scale);
            let dot_cy = y + ph / 2.0;
            let dot_color = if is_preferred {
                if is_selected {
                    0xFFFFFFFF
                } else {
                    colors::BTN_ACTIVE_BORDER_ARGB
                }
            } else if is_selected {
                0xAAFFFFFF
            } else {
                0x668C96A8
            };
            fill_circle(graphics, dot_cx, dot_cy, dot_r, dot_color);

            if let Some(ref fonts) = self.fonts {
                let text_left = dot_cx + dot_r + (PILL_DOT_GAP_DIP as f32 * scale);
                let text_rect = RectF {
                    X: text_left,
                    Y: y,
                    Width: (x + pw - (PILL_TEXT_RIGHT_PAD_DIP as f32 * scale) - text_left).max(10.0),
                    Height: ph,
                };
                let wide: Vec<u16> = variant.name.encode_utf16().collect();
                let mut text_brush: *mut GpSolidFill = std::ptr::null_mut();
                if GdipCreateSolidFill(text_argb, &mut text_brush) == GpOk && !text_brush.is_null() {
                    let font = if is_selected || is_preferred {
                        fonts.font_bold
                    } else {
                        fonts.font_regular
                    };
                    let _ = GdipDrawString(
                        graphics,
                        windows::core::PCWSTR(wide.as_ptr()),
                        wide.len() as i32,
                        font,
                        &text_rect,
                        fonts.format_left,
                        text_brush as *mut GpBrush,
                    );
                    let _ = GdipDeleteBrush(text_brush as *mut GpBrush);
                }
            }
        }
    }
}

impl Drop for VariantFanWindow {
    fn drop(&mut self) {
        self.fonts = None;
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

unsafe fn draw_circle_outline(
    graphics: *mut GpGraphics,
    cx: f32,
    cy: f32,
    radius: f32,
    argb: u32,
    stroke: f32,
) {
    let mut pen: *mut GpPen = std::ptr::null_mut();
    if GdipCreatePen1(argb, stroke, UnitPixel, &mut pen) == GpOk && !pen.is_null() {
        let _ = GdipDrawEllipse(graphics, pen, cx - radius, cy - radius, radius * 2.0, radius * 2.0);
        let _ = GdipDeletePen(pen);
    }
}

unsafe fn fill_circle(graphics: *mut GpGraphics, cx: f32, cy: f32, radius: f32, argb: u32) {
    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(argb, &mut brush) == GpOk && !brush.is_null() {
        let _ = GdipFillEllipse(
            graphics,
            brush as *mut GpBrush,
            cx - radius,
            cy - radius,
            radius * 2.0,
            radius * 2.0,
        );
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
    }
}

unsafe extern "system" fn fan_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{MA_NOACTIVATE, WM_MOUSEACTIVATE};
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RectData, SceneVariant};
    use crate::launcher::variant_fan::geometry::{FanAxis, FanLayout};

    #[test]
    fn fan_window_measures_and_renders_offscreen_without_focus() {
        let mut scene = Scene::default();
        scene.id = "code".into();
        scene.name = "Code".into();
        scene.variants = vec![
            SceneVariant {
                id: "a".into(),
                name: "Antigravity".into(),
            },
            SceneVariant {
                id: "b".into(),
                name: "Codex".into(),
            },
        ];
        let ctx = GdiplusContext::init();
        let mut win = VariantFanWindow::create(ctx).unwrap();
        let widths = win.measure_label_widths(&scene, 96);
        let work = RectData {
            left: -20000,
            top: -20000,
            right: -18976,
            bottom: -19232,
        };
        let anchor = RectData {
            left: -19500,
            top: -19500,
            right: -19400,
            bottom: -19440,
        };
        let layout = FanLayout::compute(anchor, &widths, work, FanAxis::Vertical, 96);
        win.render(&scene, &layout, Some(0), Some(0), 96);
        assert!(win.visible);
        win.hide();
        assert!(!win.visible);
    }

    #[test]
    fn variant_labels_fit_without_clipping_in_regular_and_bold() {
        let mut scene = Scene::default();
        for name in [
            "Antigravity",
            "codex",
            "Codex",
            "Claude Code",
            "Cursor",
            "日本語構成名",
            "作業用 (メイン)",
            "WWWWWWWWWW",
            "fff",
            "yyy",
            "xxx",
        ] {
            scene.variants.push(SceneVariant {
                id: name.into(),
                name: name.into(),
            });
        }
        let ctx = GdiplusContext::init();
        let mut win = VariantFanWindow::create(ctx).unwrap();
        let work = RectData {
            left: 0,
            top: 0,
            right: 2560,
            bottom: 1440,
        };
        let anchor = RectData {
            left: 1000,
            top: 1000,
            right: 1040,
            bottom: 1040,
        };

        for dpi in [96u32, 120, 144, 192] {
            let widths = win.measure_label_widths(&scene, dpi);
            let layout = FanLayout::compute(anchor, &widths, work, FanAxis::Vertical, dpi);
            let fonts = win.fonts.as_ref().unwrap();
            let scale = dpi as f32 / 96.0;
            let dot_r = (PILL_DOT_RADIUS_DIP as f32 * scale).max(2.2);
            let left_consumed = (PILL_DOT_CX_DIP as f32 * scale) + dot_r + (PILL_DOT_GAP_DIP as f32 * scale);
            let right_consumed = PILL_TEXT_RIGHT_PAD_DIP as f32 * scale;

            unsafe {
                let mem_dc = CreateCompatibleDC(None);
                let mut graphics: *mut GpGraphics = std::ptr::null_mut();
                assert_eq!(GdipCreateFromHDC(mem_dc, &mut graphics), GpOk);
                let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
                let _ = GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAliasGridFit);

                for (i, v) in scene.variants.iter().enumerate() {
                    let pw = layout.items[i].rect.width() as f32;
                    let ph = layout.items[i].rect.height() as f32;
                    let avail_w = (pw - left_consumed - right_consumed).max(10.0);
                    let fit_rect = RectF {
                        X: 0.0,
                        Y: 0.0,
                        Width: avail_w,
                        Height: ph,
                    };
                    let wide: Vec<u16> = v.name.encode_utf16().collect();
                    for (style, font) in [("regular", fonts.font_regular), ("bold", fonts.font_bold)] {
                        let mut bound = RectF::default();
                        let mut codepoints_fitted = 0i32;
                        let status = GdipMeasureString(
                            graphics,
                            windows::core::PCWSTR(wide.as_ptr()),
                            wide.len() as i32,
                            font,
                            &fit_rect,
                            fonts.format_left,
                            &mut bound,
                            &mut codepoints_fitted,
                            std::ptr::null_mut(),
                        );
                        assert_eq!(status, GpOk);
                        assert_eq!(
                            codepoints_fitted,
                            wide.len() as i32,
                            "dpi={dpi} style={style} name={:?} clipped ({codepoints_fitted}/{}) in avail_w={avail_w}",
                            v.name,
                            wide.len()
                        );
                    }
                }
                let _ = GdipDeleteGraphics(graphics);
                let _ = DeleteDC(mem_dc);
            }
        }
    }
}
