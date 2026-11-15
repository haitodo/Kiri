//! Paint the cursor launcher's panel, scene tiles, and shortcut rows.

use super::{win_rect, CursorPopup, Hit, CORNER_RADIUS_DIP};
use crate::config::{RectData, ShortcutKind};
use crate::platform::monitors::dip_to_px;
use crate::platform::windows::apps_use_dark_theme;
use std::ptr::null_mut;
use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, FillRect, GetStockObject, SelectObject, SetBkMode, SetDCBrushColor, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DC_BRUSH, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_NORMAL, HBRUSH, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SRCCOPY, TRANSPARENT,
};
use windows::Win32::Graphics::GdiPlus::{
    DashCapRound, FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC,
    GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics,
    GdipDeletePath, GdipDeletePen, GdipDrawImageRect, GdipDrawLines, GdipFillPath,
    GdipFillRectangleI, GdipSetInterpolationMode, GdipSetPenLineCap197819, GdipSetPenLineJoin,
    GdipSetSmoothingMode, GdipSetSolidFillColor, GpGraphics, GpSolidFill,
    InterpolationModeHighQualityBicubic, LineCapRound, LineJoinRound, Ok as GpOk, PointF,
    SmoothingModeAntiAlias, UnitPixel,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PopupPalette {
    surface: u32,
    tile: u32,
    text: u32,
    muted: u32,
    border: u32,
    hover: u32,
    active: u32,
    accent: u32,
    active_border: u32,
}

impl PopupPalette {
    const LIGHT: Self = Self {
        surface: 0xfcfcfd,
        tile: 0xf3f5f8,
        text: 0x1b2635,
        muted: 0x4b5665,
        border: 0xd7dde6,
        hover: 0xeaf0f7,
        active: 0xe3eeff,
        accent: 0x0f52a0,
        active_border: 0x91b3e2,
    };
    const DARK: Self = Self {
        surface: 0x20242c,
        tile: 0x282e38,
        text: 0xf2f5fb,
        muted: 0xb7c2d1,
        border: 0x3b4451,
        hover: 0x303b4a,
        active: 0x283f60,
        accent: 0xbdd8ff,
        active_border: 0x5b83bc,
    };

    pub(super) fn system() -> Self {
        if apps_use_dark_theme() {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

pub(super) struct PopupFonts {
    body: HFONT,
    tile: HFONT,
}

impl PopupFonts {
    pub(super) fn new(dpi: u32) -> Self {
        let create = |size, weight| unsafe {
            CreateFontW(
                -dip_to_px(size, dpi),
                0,
                0,
                0,
                weight,
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
        };
        Self {
            body: create(12, FW_NORMAL.0 as i32),
            tile: create(11, FW_NORMAL.0 as i32),
        }
    }
}

impl Drop for PopupFonts {
    fn drop(&mut self) {
        for font in [self.body, self.tile] {
            if !font.is_invalid() {
                let _ = unsafe { DeleteObject(font.into()) };
            }
        }
    }
}

impl CursorPopup {
    pub(super) unsafe fn paint(&mut self, hdc: HDC) {
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        let dc = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, self.width, self.height);
        let buffered = !dc.is_invalid() && !bitmap.is_invalid();
        let target = if buffered { dc } else { hdc };
        let old_bitmap = if buffered {
            SelectObject(dc, bitmap.into())
        } else {
            HGDIOBJ::default()
        };

        let frame = RectData {
            right: self.window_width,
            bottom: self.height,
            ..Default::default()
        };
        let _ = SetDCBrushColor(target, colorref(self.palette.surface));
        FillRect(target, &win_rect(frame), HBRUSH(GetStockObject(DC_BRUSH).0));
        if let Some(canvas) = Canvas::new(target) {
            self.paint_shapes(&canvas, frame);
        }
        self.paint_labels(target);

        if buffered {
            let _ = BitBlt(hdc, 0, 0, self.width, self.height, Some(dc), 0, 0, SRCCOPY);
            let _ = SelectObject(dc, old_bitmap);
        }
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
    }

    unsafe fn paint_shapes(&mut self, canvas: &Canvas, frame: RectData) {
        let px = |dip| dip_to_px(dip, self.dpi);
        let palette = self.palette;
        let border = px(1);
        let radius = px(CORNER_RADIUS_DIP);
        canvas.fill(frame, radius, palette.border);
        canvas.fill(inset(frame, border), radius - border, palette.surface);

        if let Some(top) = self.layout.divider_top {
            if let Some(rect) = self.viewport_rect(RectData {
                left: self.layout.pad,
                right: self.width - self.layout.pad,
                top,
                bottom: top + border,
            }) {
                canvas.fill(rect, 0, palette.border);
            }
        }

        let icon_size = px(14);
        let tile_radius = px(5);
        for (index, scene) in self.scenes.iter().enumerate() {
            let Some(rect) = self.viewport_rect(self.layout.scene(index, self.width)) else {
                continue;
            };
            let active = self.active_scene_id.as_deref() == Some(scene.id.as_str());
            let focused = self.keyboard_focus == Some(Hit::Scene(index));
            if active {
                canvas.fill(rect, tile_radius, palette.active_border);
                canvas.fill(inset(rect, border), tile_radius - border, palette.active);
            } else if focused {
                canvas.fill(rect, tile_radius, palette.active_border);
                canvas.fill(inset(rect, border), tile_radius - border, palette.hover);
            } else {
                let fill = if self.hover == Some(Hit::Scene(index)) {
                    palette.hover
                } else {
                    palette.tile
                };
                canvas.fill(rect, tile_radius, fill);
            }

            let x = rect.left + (rect.width() - icon_size) / 2;
            let y = rect.top + px(6);
            if let Some(image) = self.icon_cache.get_or_load_image(&scene.icon, self.dpi) {
                let _ = GdipDrawImageRect(
                    canvas.graphics,
                    image,
                    x as f32,
                    y as f32,
                    icon_size as f32,
                    icon_size as f32,
                );
            } else {
                let color = 0xFF00_0000 | if active { palette.accent } else { palette.text };
                crate::launcher::paint::draw_builtin_icon_gdiplus(
                    canvas.graphics,
                    x as f32 + icon_size as f32 / 2.0,
                    y as f32 + icon_size as f32 / 2.0,
                    icon_size as f32 * 0.45,
                    self.dpi,
                    &scene.icon,
                    color,
                );
            }
            if scene.variants.len() > 1 {
                let badge_color = 0xB800_0000 | if active { palette.accent } else { palette.muted };
                let badge_rect = RectData {
                    bottom: rect.top + px(11),
                    ..rect
                };
                crate::launcher::paint::draw_variant_badge(
                    canvas.graphics,
                    badge_rect,
                    badge_color,
                    self.dpi,
                );
            }
        }

        let row_radius = px(4);
        for index in 0..self.shortcuts.len().max(1) {
            let Some(rect) = self.viewport_rect(self.layout.shortcut(index, self.width)) else {
                continue;
            };
            let shortcut = self.shortcuts.get(index);
            let hit = if shortcut.is_some() {
                Hit::Shortcut(index)
            } else {
                Hit::Settings
            };
            if self.keyboard_focus == Some(hit) {
                canvas.fill(rect, row_radius, palette.active_border);
                canvas.fill(inset(rect, border), row_radius - border, palette.hover);
            } else if self.hover == Some(hit) {
                canvas.fill(rect, row_radius, palette.hover);
            }
            if let Some(shortcut) = shortcut {
                let icon = match shortcut.kind {
                    ShortcutKind::Folder => Icon::Folder,
                    ShortcutKind::File => Icon::File,
                    ShortcutKind::Website => Icon::Website,
                };
                let x = rect.left + px(6);
                let y = rect.top + (rect.height() - icon_size) / 2;
                canvas.icon(icon, x, y, icon_size, palette.muted);
            }
        }
    }

    unsafe fn paint_labels(&self, hdc: HDC) {
        let px = |dip| dip_to_px(dip, self.dpi);
        let palette = self.palette;
        let _ = SetBkMode(hdc, TRANSPARENT);
        let old_font = select_font(hdc, self.fonts.tile);

        for (index, scene) in self.scenes.iter().enumerate() {
            let Some(rect) = self.viewport_rect(self.layout.scene(index, self.width)) else {
                continue;
            };
            let active = self.active_scene_id.as_deref() == Some(scene.id.as_str());
            let color = if active { palette.accent } else { palette.text };
            draw_text(
                hdc,
                &scene.name,
                RectData {
                    left: rect.left + px(3),
                    right: rect.right - px(3),
                    top: rect.top + px(22),
                    bottom: rect.top + px(38),
                },
                DT_CENTER,
                color,
            );
        }

        select_font(hdc, self.fonts.body);
        for index in 0..self.shortcuts.len().max(1) {
            let Some(rect) = self.viewport_rect(self.layout.shortcut(index, self.width)) else {
                continue;
            };
            if let Some(shortcut) = self.shortcuts.get(index) {
                let name = RectData {
                    left: rect.left + px(25),
                    right: rect.right - px(6),
                    ..rect
                };
                draw_text(hdc, &shortcut.name, name, DT_LEFT, palette.text);
            } else {
                draw_text(
                    hdc,
                    "ショートカットを追加…",
                    RectData {
                        left: rect.left + px(6),
                        right: rect.right - px(6),
                        ..rect
                    },
                    DT_LEFT,
                    palette.text,
                );
            }
        }
        let _ = SelectObject(hdc, old_font);
    }
}

fn colorref(rgb: u32) -> COLORREF {
    COLORREF((rgb >> 16) | (rgb & 0xff00) | ((rgb & 0xff) << 16))
}

fn inset(rect: RectData, amount: i32) -> RectData {
    RectData {
        left: rect.left + amount,
        top: rect.top + amount,
        right: rect.right - amount,
        bottom: rect.bottom - amount,
    }
}

unsafe fn select_font(hdc: HDC, font: HFONT) -> HGDIOBJ {
    let font = if font.is_invalid() {
        GetStockObject(DEFAULT_GUI_FONT)
    } else {
        font.into()
    };
    SelectObject(hdc, font)
}

unsafe fn draw_text(hdc: HDC, text: &str, rect: RectData, alignment: DRAW_TEXT_FORMAT, rgb: u32) {
    if rect.width() <= 0 {
        return;
    }
    let mut text: Vec<u16> = text.encode_utf16().collect();
    let _ = SetTextColor(hdc, colorref(rgb));
    let _ = DrawTextW(
        hdc,
        &mut text,
        &mut win_rect(rect),
        alignment | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
}

struct Canvas {
    graphics: *mut GpGraphics,
    brush: *mut GpSolidFill,
}

impl Canvas {
    unsafe fn new(hdc: HDC) -> Option<Self> {
        let mut graphics = null_mut();
        if GdipCreateFromHDC(hdc, &mut graphics) != GpOk {
            return None;
        }
        let mut brush = null_mut();
        if GdipCreateSolidFill(0xff000000, &mut brush) != GpOk {
            let _ = GdipDeleteGraphics(graphics);
            return None;
        }
        let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        let _ = GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);
        Some(Self { graphics, brush })
    }

    unsafe fn fill(&self, rect: RectData, radius: i32, rgb: u32) {
        if rect.width() <= 0 || rect.height() <= 0 {
            return;
        }
        let _ = GdipSetSolidFillColor(self.brush, 0xff000000 | rgb);
        if radius <= 0 {
            let _ = GdipFillRectangleI(
                self.graphics,
                self.brush.cast(),
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
            );
            return;
        }
        let mut path = null_mut();
        if GdipCreatePath(FillModeAlternate, &mut path) != GpOk {
            return;
        }
        let diameter = (radius * 2).min(rect.width()).min(rect.height()) as f32;
        let left = rect.left as f32;
        let top = rect.top as f32;
        let right = rect.right as f32 - diameter;
        let bottom = rect.bottom as f32 - diameter;
        for (x, y, start) in [
            (left, top, 180.0),
            (right, top, 270.0),
            (right, bottom, 0.0),
            (left, bottom, 90.0),
        ] {
            let _ = GdipAddPathArc(path, x, y, diameter, diameter, start, 90.0);
        }
        let _ = GdipClosePathFigure(path);
        let _ = GdipFillPath(self.graphics, self.brush.cast(), path);
        let _ = GdipDeletePath(path);
    }

    unsafe fn icon(&self, icon: Icon, x: i32, y: i32, size: i32, rgb: u32) {
        let scale = size as f32 / 24.0;
        let mut pen = null_mut();
        if GdipCreatePen1(0xff000000 | rgb, 2.0 * scale, UnitPixel, &mut pen) != GpOk {
            return;
        }
        let _ = GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapRound);
        let _ = GdipSetPenLineJoin(pen, LineJoinRound);
        let line = |points: &[(f32, f32)]| {
            let points: Vec<_> = points
                .iter()
                .map(|&(px, py)| PointF {
                    X: x as f32 + px * scale,
                    Y: y as f32 + py * scale,
                })
                .collect();
            let _ = GdipDrawLines(self.graphics, pen, points.as_ptr(), points.len() as i32);
        };
        match icon {
            Icon::Folder => {
                line(&[
                    (3.0, 7.0),
                    (5.0, 5.0),
                    (9.0, 5.0),
                    (11.0, 7.0),
                    (19.0, 7.0),
                    (21.0, 9.0),
                    (21.0, 18.0),
                    (19.0, 20.0),
                    (5.0, 20.0),
                    (3.0, 18.0),
                    (3.0, 7.0),
                ]);
            }
            Icon::File => {
                line(&[
                    (6.0, 3.0),
                    (14.0, 3.0),
                    (19.0, 8.0),
                    (19.0, 21.0),
                    (6.0, 21.0),
                    (6.0, 3.0),
                ]);
                line(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)]);
                line(&[(9.0, 13.0), (16.0, 13.0)]);
                line(&[(9.0, 17.0), (16.0, 17.0)]);
            }
            Icon::Website => {
                line(&[
                    (13.0, 5.0),
                    (6.0, 5.0),
                    (4.0, 7.0),
                    (4.0, 18.0),
                    (6.0, 20.0),
                    (17.0, 20.0),
                    (19.0, 18.0),
                    (19.0, 12.0),
                ]);
                line(&[(14.0, 3.0), (21.0, 3.0), (21.0, 10.0)]);
                line(&[(10.0, 14.0), (21.0, 3.0)]);
            }
        }
        let _ = GdipDeletePen(pen);
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            let _ = GdipDeleteBrush(self.brush.cast());
            let _ = GdipDeleteGraphics(self.graphics);
        }
    }
}

enum Icon {
    Folder,
    File,
    Website,
}
