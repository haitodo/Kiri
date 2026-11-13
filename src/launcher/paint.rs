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

use crate::config::{Orientation, RectData};
use crate::launcher::input::{HitTarget, LauncherLayout, CORNER_RADIUS_DIP};
use crate::platform::monitors::dip_to_px;
use crate::scenes::manager::SceneState;
use std::collections::HashMap;
use std::sync::Arc;

/// Built-in vector icon presets `(id, japanese_label)`.
pub const BUILTIN_ICONS: &[(&str, &str)] = &[
    ("chart", "チャート / トレード"),
    ("code", "コード / 開発"),
    ("globe", "ブラウザ / Web"),
    ("terminal", "ターミナル / CLI"),
    ("folder", "フォルダ / 資料"),
    ("mail", "メール"),
    ("chat", "チャット / 通話"),
    ("star", "お気に入り / 汎用"),
];

pub fn is_builtin_icon(icon_name: &str) -> bool {
    BUILTIN_ICONS.iter().any(|(id, _)| *id == icon_name)
}

pub mod colors {
    pub const BG_ARGB: u32 = 0xFF181A1F;
    pub const BORDER_ARGB: u32 = 0xFF343944;
    pub const BTN_HOVER_ARGB: u32 = 0xFF2A2E38;
    pub const BTN_PRESSED_ARGB: u32 = 0xFF21252E;
    pub const BTN_ACTIVE_ARGB: u32 = 0xFF214C8A;
    pub const BTN_ACTIVE_PRESSED_ARGB: u32 = 0xFF1A3D70;
    pub const BTN_ACTIVE_BORDER_ARGB: u32 = 0xFF4C94F6;
    pub const SUBTLE_HINT_ARGB: u32 = 0xFF5A739B;
    pub const TEXT_PRIMARY_ARGB: u32 = 0xFFEAEFF7;
    pub const TEXT_MUTED_ARGB: u32 = 0xFF8C96A8;
}

#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, SelectObject, HBRUSH, HDC, HGDIOBJ, SRCCOPY,
};
#[cfg(windows)]
use windows::Win32::Graphics::GdiPlus::{
    DashCapRound, FillModeAlternate, GdipAddPathArc, GdipClosePathFigure,
    GdipCreateBitmapFromHICON, GdipCreateBitmapFromScan0, GdipCreateFromHDC,
    GdipCreateHICONFromBitmap, GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill,
    GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDisposeImage,
    GdipDrawEllipse, GdipDrawImageRect, GdipDrawLine, GdipDrawPath, GdipDrawRectangle,
    GdipFillEllipse, GdipFillPath, GdipGetImageGraphicsContext, GdipGraphicsClear,
    GdipLoadImageFromFile, GdipSetInterpolationMode, GdipSetPenLineCap197819, GdipSetPenLineJoin,
    GdipSetSmoothingMode, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpBrush,
    GpGraphics, GpImage, GpPath, GpPen, GpSolidFill, InterpolationModeHighQualityBicubic,
    LineCapRound, LineJoinRound, Ok as GpOk, SmoothingModeAntiAlias, UnitPixel,
};

#[cfg(windows)]
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;
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
        if icon_source.is_empty() || is_builtin_icon(icon_source) {
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
    pub pressed: &'a HitTarget,
    pub has_warnings: bool,
}

#[cfg(windows)]
unsafe fn create_rounded_rect_path(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
) -> Option<*mut GpPath> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let mut path: *mut GpPath = std::ptr::null_mut();
    if GdipCreatePath(FillModeAlternate, &mut path) != GpOk || path.is_null() {
        return None;
    }
    let r = radius.clamp(0.5, (w.min(h)) / 2.0);
    let d = r * 2.0;
    let _ = GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    let _ = GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    let _ = GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    let _ = GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    let _ = GdipClosePathFigure(path);
    Some(path)
}

#[cfg(windows)]
pub unsafe fn fill_rounded_rect_gdiplus(
    graphics: *mut GpGraphics,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    argb: u32,
) {
    let Some(path) = create_rounded_rect_path(x, y, w, h, radius) else {
        return;
    };
    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(argb, &mut brush) == GpOk && !brush.is_null() {
        let _ = GdipFillPath(graphics, brush as *mut GpBrush, path);
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
    }
    let _ = GdipDeletePath(path);
}

#[cfg(windows)]
pub unsafe fn draw_rounded_rect_border_gdiplus(
    graphics: *mut GpGraphics,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    argb: u32,
    stroke_w: f32,
) {
    let Some(path) = create_rounded_rect_path(x, y, w, h, radius) else {
        return;
    };
    let mut pen: *mut GpPen = std::ptr::null_mut();
    if GdipCreatePen1(argb, stroke_w, UnitPixel, &mut pen) == GpOk && !pen.is_null() {
        let _ = GdipSetPenLineJoin(pen, LineJoinRound);
        let _ = GdipDrawPath(graphics, pen, path);
        let _ = GdipDeletePen(pen);
    }
    let _ = GdipDeletePath(path);
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

    // 1. Dark background base (clipped to rounded region on window)
    FillRect(mem_dc, &full_rc, theme.bg_brush);

    // 2. Render rounded container border, rounded buttons, vector icons, grip dots, overflow dots, and warning indicator
    let mut graphics: *mut GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(mem_dc, &mut graphics) == GpOk && !graphics.is_null() {
        let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);

        let scale = dpi as f32 / 96.0;
        let outer_radius = (CORNER_RADIUS_DIP as f32 * scale).max(4.0);
        let btn_radius = ((CORNER_RADIUS_DIP - 1) as f32 * scale).max(3.5);
        let stroke = (1.0 * scale).max(1.0);

        // Rounded outer frame inset cleanly inside window bounds
        draw_rounded_rect_border_gdiplus(
            graphics,
            0.5,
            0.5,
            (w as f32 - 1.0).max(1.0),
            (h as f32 - 1.0).max(1.0),
            outer_radius,
            colors::BORDER_ARGB,
            stroke,
        );

        // Scene button backgrounds
        for (idx, slot) in state.layout.scene_buttons.iter().enumerate() {
            let is_active =
                matches!(state.scene_state, SceneState::Active(id) if id == &slot.scene.id);
            let is_last_hint = matches!(state.scene_state, SceneState::Unknown)
                && state.last_known_scene_id == Some(slot.scene.id.as_str());
            let is_hovered = matches!(
                state.hovered,
                HitTarget::SceneButton { index, .. } if *index == idx
            );
            let is_pressed = matches!(
                state.pressed,
                HitTarget::SceneButton { index, .. } if *index == idx
            );

            draw_button_bg_gdiplus(
                graphics,
                &slot.rect,
                btn_radius,
                is_active,
                is_hovered,
                is_pressed,
                is_last_hint,
                dpi,
            );
        }

        // Overflow button background
        if let Some(ref ov_rect) = state.layout.overflow_button_rect {
            let is_active = state.active_in_overflow;
            let is_hovered = matches!(state.hovered, HitTarget::OverflowButton);
            let is_pressed = matches!(state.pressed, HitTarget::OverflowButton);
            draw_button_bg_gdiplus(
                graphics,
                ov_rect,
                btn_radius,
                is_active,
                is_hovered,
                is_pressed,
                false,
                dpi,
            );
        }

        // Grip dots (`⠿`, rotated cleanly according to bar orientation)
        draw_grip_dots_gdiplus(graphics, &state.layout.grip_rect, state.layout.orientation, dpi);

        // Scene icons
        for slot in &state.layout.scene_buttons {
            let is_active =
                matches!(state.scene_state, SceneState::Active(id) if id == &slot.scene.id);
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
unsafe fn draw_grip_dots_gdiplus(
    graphics: *mut GpGraphics,
    rect: &RectData,
    orientation: Orientation,
    dpi: u32,
) {
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let dot_r = (1.3 * dpi as f32 / 96.0).max(1.0);
    let step_minor = (4.0 * dpi as f32 / 96.0).max(3.0);
    let step_major = (5.0 * dpi as f32 / 96.0).max(4.0);

    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    if GdipCreateSolidFill(0xFF6E7687, &mut brush) == GpOk {
        match orientation {
            Orientation::Horizontal => {
                // 2 cols x 3 rows (vertical grip strip on left)
                for col in [-1.0f32, 1.0f32] {
                    for row in [-1.0f32, 0.0f32, 1.0f32] {
                        let x = cx + (col * step_minor) / 2.0;
                        let y = cy + row * step_major;
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
            }
            Orientation::Vertical => {
                // 3 cols x 2 rows (horizontal grip strip on top)
                for col in [-1.0f32, 0.0f32, 1.0f32] {
                    for row in [-1.0f32, 1.0f32] {
                        let x = cx + col * step_major;
                        let y = cy + (row * step_minor) / 2.0;
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
            }
        }
        let _ = GdipDeleteBrush(brush as *mut GpBrush);
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn draw_button_bg_gdiplus(
    graphics: *mut GpGraphics,
    rect: &RectData,
    radius: f32,
    is_active: bool,
    is_hovered: bool,
    is_pressed: bool,
    is_last_hint: bool,
    dpi: u32,
) {
    let x = rect.left as f32 + 0.5;
    let y = rect.top as f32 + 0.5;
    let w = (rect.right - rect.left) as f32 - 1.0;
    let h = (rect.bottom - rect.top) as f32 - 1.0;
    let stroke = (1.0 * dpi as f32 / 96.0).max(1.0);

    if is_active {
        let fill = if is_pressed {
            colors::BTN_ACTIVE_PRESSED_ARGB
        } else {
            colors::BTN_ACTIVE_ARGB
        };
        fill_rounded_rect_gdiplus(graphics, x, y, w, h, radius, fill);
        draw_rounded_rect_border_gdiplus(
            graphics,
            x,
            y,
            w,
            h,
            radius,
            colors::BTN_ACTIVE_BORDER_ARGB,
            stroke,
        );
    } else if is_pressed {
        fill_rounded_rect_gdiplus(graphics, x, y, w, h, radius, colors::BTN_PRESSED_ARGB);
        draw_rounded_rect_border_gdiplus(graphics, x, y, w, h, radius, colors::BORDER_ARGB, stroke);
    } else if is_hovered {
        fill_rounded_rect_gdiplus(graphics, x, y, w, h, radius, colors::BTN_HOVER_ARGB);
        draw_rounded_rect_border_gdiplus(graphics, x, y, w, h, radius, colors::BORDER_ARGB, stroke);
    }

    // Subtle bottom bar when state is Unknown and this was the previous scene (Section 2.1 & 12)
    if !is_active && is_last_hint {
        let bar_h = (2.0 * dpi as f32 / 96.0).max(1.5);
        let inset = (8.0 * dpi as f32 / 96.0).max(4.0);
        let hint_w = ((rect.right - rect.left) as f32 - inset * 2.0).max(4.0);
        fill_rounded_rect_gdiplus(
            graphics,
            rect.left as f32 + inset,
            rect.bottom as f32 - bar_h - 1.5,
            hint_w,
            bar_h,
            bar_h / 2.0,
            colors::SUBTLE_HINT_ARGB,
        );
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
        let max_dim = ((rect.right - rect.left).min(rect.bottom - rect.top) as f32 * 0.62)
            .clamp(12.0, 26.0 * dpi as f32 / 96.0);
        let _ = GdipDrawImageRect(
            graphics,
            img,
            cx - max_dim / 2.0,
            cy - max_dim / 2.0,
            max_dim,
            max_dim,
        );
        return;
    }

    let btn_min = (rect.right - rect.left).min(rect.bottom - rect.top) as f32;
    let half = (btn_min * 0.25).clamp(5.5 * dpi as f32 / 96.0, 10.0 * dpi as f32 / 96.0);
    let color = if is_active {
        0xFFFFFFFF // Crisp pure white
    } else {
        0xFFCDD4E0 // rgb(205, 212, 224)
    };
    let _ = scene_name;
    draw_builtin_icon_gdiplus(graphics, cx, cy, half, dpi, icon_name, color);
}

/// Shared antialiased built-in vector icon renderer used by Fixed Bar, OverflowPopup, and CursorPopup.
#[cfg(windows)]
pub unsafe fn draw_builtin_icon_gdiplus(
    graphics: *mut GpGraphics,
    cx: f32,
    cy: f32,
    half: f32,
    dpi: u32,
    icon_name: &str,
    color: u32,
) {
    let stroke_w = (1.75 * dpi as f32 / 96.0).clamp(1.4, 2.6);
    let mut pen: *mut GpPen = std::ptr::null_mut();
    if GdipCreatePen1(color, stroke_w, UnitPixel, &mut pen) != GpOk {
        return;
    }
    let _ = GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapRound);
    let _ = GdipSetPenLineJoin(pen, LineJoinRound);

    match icon_name {
        "chart" => {
            // Rising trend line + arrowhead
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

            let arrow = half * 0.44;
            let _ = GdipDrawLine(graphics, pen, x4 - arrow, y4, x4, y4);
            let _ = GdipDrawLine(graphics, pen, x4, y4, x4, y4 + arrow);
        }
        "code" => {
            // `< / >` brackets
            let w_brk = half * 0.44;
            let h_brk = half * 0.62;

            let _ = GdipDrawLine(graphics, pen, cx - w_brk, cy - h_brk, cx - half, cy);
            let _ = GdipDrawLine(graphics, pen, cx - half, cy, cx - w_brk, cy + h_brk);

            let _ = GdipDrawLine(graphics, pen, cx + w_brk, cy - h_brk, cx + half, cy);
            let _ = GdipDrawLine(graphics, pen, cx + half, cy, cx + w_brk, cy + h_brk);

            let slash_x = half * 0.22;
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
            let _ = GdipDrawEllipse(graphics, pen, cx - half, cy - half, half * 2.0, half * 2.0);
            let _ = GdipDrawLine(graphics, pen, cx - half, cy, cx + half, cy);
            let v_half = half * 0.45;
            let _ = GdipDrawEllipse(
                graphics,
                pen,
                cx - v_half,
                cy - half,
                v_half * 2.0,
                half * 2.0,
            );
        }
        "terminal" => {
            // Rounded-ish prompt box with `>_`
            let bw = half * 1.9;
            let bh = half * 1.55;
            let left = cx - bw / 2.0;
            let top = cy - bh / 2.0;
            let _ = GdipDrawRectangle(graphics, pen, left, top, bw, bh);
            let px = cx - half * 0.52;
            let py = cy;
            let _ = GdipDrawLine(graphics, pen, px, py - half * 0.28, px + half * 0.34, py);
            let _ = GdipDrawLine(graphics, pen, px + half * 0.34, py, px, py + half * 0.28);
            let _ = GdipDrawLine(
                graphics,
                pen,
                cx + half * 0.08,
                cy + half * 0.28,
                cx + half * 0.55,
                cy + half * 0.28,
            );
        }
        "folder" => {
            let left = cx - half;
            let right = cx + half;
            let top = cy - half * 0.65;
            let bottom = cy + half * 0.72;
            let tab_mid = cx - half * 0.22;
            let notch_y = top + half * 0.32;
            let _ = GdipDrawLine(graphics, pen, left, top, tab_mid, top);
            let _ = GdipDrawLine(graphics, pen, tab_mid, top, tab_mid + half * 0.22, notch_y);
            let _ = GdipDrawLine(graphics, pen, tab_mid + half * 0.22, notch_y, right, notch_y);
            let _ = GdipDrawLine(graphics, pen, right, notch_y, right, bottom);
            let _ = GdipDrawLine(graphics, pen, right, bottom, left, bottom);
            let _ = GdipDrawLine(graphics, pen, left, bottom, left, top);
        }
        "mail" => {
            let bw = half * 1.95;
            let bh = half * 1.4;
            let left = cx - bw / 2.0;
            let top = cy - bh / 2.0;
            let _ = GdipDrawRectangle(graphics, pen, left, top, bw, bh);
            let _ = GdipDrawLine(graphics, pen, left, top, cx, cy + half * 0.1);
            let _ = GdipDrawLine(graphics, pen, left + bw, top, cx, cy + half * 0.1);
        }
        "chat" => {
            let left = cx - half;
            let top = cy - half * 0.75;
            let bw = half * 2.0;
            let bh = half * 1.35;
            let _ = GdipDrawRectangle(graphics, pen, left, top, bw, bh);
            let _ = GdipDrawLine(
                graphics,
                pen,
                cx - half * 0.45,
                top + bh,
                cx - half * 0.55,
                cy + half * 0.9,
            );
            let _ = GdipDrawLine(
                graphics,
                pen,
                cx - half * 0.55,
                cy + half * 0.9,
                cx - half * 0.05,
                top + bh,
            );
        }
        "star" => {
            // Crisp 4-point sparkle + small orbit or diamond star
            let r_outer = half;
            let r_inner = half * 0.38;
            let mut pts = [(0.0f32, 0.0f32); 10];
            for (i, pt) in pts.iter_mut().enumerate() {
                let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
                let r = if i % 2 == 0 { r_outer } else { r_inner };
                *pt = (cx + r * angle.cos(), cy + r * angle.sin());
            }
            for i in 0..10 {
                let (x1, y1) = pts[i];
                let (x2, y2) = pts[(i + 1) % 10];
                let _ = GdipDrawLine(graphics, pen, x1, y1, x2, y2);
            }
        }
        _ => {
            // Fallback outline circle
            let _ = GdipDrawEllipse(graphics, pen, cx - half, cy - half, half * 2.0, half * 2.0);
        }
    }

    let _ = GdipDeletePen(pen);
}

/// Generates a crisp, DPI-sized custom Kiri application icon (`HICON`) using GDI+.
/// Caller owns the returned `HICON` and must call `DestroyIcon` when done.
#[cfg(windows)]
pub fn create_kiri_hicon(size_px: i32) -> Option<HICON> {
    let size = size_px.clamp(16, 128);
    // Ensure GDI+ is initialized for callers outside LauncherWindowHost
    let _ctx = GdiplusContext::init();
    unsafe {
        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromScan0(
            size,
            size,
            0,
            PIXEL_FORMAT_32BPP_PARGB,
            None,
            &mut bitmap,
        ) != GpOk
            || bitmap.is_null()
        {
            return None;
        }

        let mut graphics: *mut GpGraphics = std::ptr::null_mut();
        if GdipGetImageGraphicsContext(bitmap as *mut GpImage, &mut graphics) == GpOk
            && !graphics.is_null()
        {
            let _ = GdipGraphicsClear(graphics, 0x00000000);
            let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);

            let s = size as f32;
            let pad = (s * 0.06).max(1.0);
            let rect_s = s - pad * 2.0;
            let r = (s * 0.22).max(2.5);

            // Dark slate rounded square base + crisp accent border
            fill_rounded_rect_gdiplus(graphics, pad, pad, rect_s, rect_s, r, 0xFF181A1F);
            draw_rounded_rect_border_gdiplus(
                graphics,
                pad,
                pad,
                rect_s,
                rect_s,
                r,
                colors::BTN_ACTIVE_BORDER_ARGB,
                (s * 0.075).max(1.2),
            );

            // Inner layered workspace cards: left slate tile + active blue tile
            let t_w = rect_s * 0.32;
            let t_h = rect_s * 0.56;
            let gap = rect_s * 0.09;
            let start_x = s / 2.0 - t_w - gap / 2.0;
            let start_y = (s - t_h) / 2.0;
            let t_r = (s * 0.09).max(1.5);

            fill_rounded_rect_gdiplus(
                graphics,
                start_x,
                start_y,
                t_w,
                t_h * 0.45,
                t_r,
                0xFF7B93B8,
            );
            fill_rounded_rect_gdiplus(
                graphics,
                start_x,
                start_y + t_h * 0.55,
                t_w,
                t_h * 0.45,
                t_r,
                0xFF4C94F6,
            );
            fill_rounded_rect_gdiplus(
                graphics,
                start_x + t_w + gap,
                start_y,
                t_w,
                t_h,
                t_r,
                0xFFEAF2FF,
            );

            let _ = GdipDeleteGraphics(graphics);
        }

        let mut hicon = HICON::default();
        let status = GdipCreateHICONFromBitmap(bitmap, &mut hicon);
        let _ = GdipDisposeImage(bitmap as *mut GpImage);

        if status == GpOk && !hicon.0.is_null() {
            Some(hicon)
        } else {
            None
        }
    }
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

                for (id, _) in BUILTIN_ICONS {
                    draw_scene_icon(graphics, &mut theme, &rect, id, "Test", true, dpi);
                    draw_scene_icon(graphics, &mut theme, &rect, id, "Test", false, dpi);
                }
                draw_scene_icon(
                    graphics,
                    &mut theme,
                    &rect,
                    "custom_fallback",
                    "Test",
                    false,
                    dpi,
                );

                draw_grip_dots_gdiplus(graphics, &rect, Orientation::Horizontal, dpi);
                draw_grip_dots_gdiplus(graphics, &rect, Orientation::Vertical, dpi);
                draw_overflow_dots_gdiplus(graphics, &rect, true, dpi);
                draw_overflow_dots_gdiplus(graphics, &rect, false, dpi);
                draw_warn_dot_gdiplus(graphics, w, dpi);

                let _ = GdipDeleteGraphics(graphics);
                let _ = SelectObject(hdc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
            }

            let _ = DeleteDC(hdc);

            let hicon = create_kiri_hicon(32).expect("create_kiri_hicon should succeed");
            assert!(!hicon.0.is_null());
            let _ = DestroyIcon(hicon);
        }
    }
}


