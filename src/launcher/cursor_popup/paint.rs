//! Paint the cursor launcher's panel, scene tiles, and shortcut rows.

use super::{win_rect, CursorPopup, Hit, CORNER_RADIUS_DIP};
use crate::config::{RectData, ShortcutKind};
use crate::platform::monitors::dip_to_px;
use std::mem::size_of;
use std::ptr::null_mut;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, FillRect, GetStockObject, GetTextExtentPoint32W, SelectObject, SetBkMode,
    SetDCBrushColor, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DC_BRUSH,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, HBRUSH,
    HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SRCCOPY, TRANSPARENT,
};
use windows::Win32::Graphics::GdiPlus::{
    DashCapRound, FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC,
    GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics,
    GdipDeletePath, GdipDeletePen, GdipDrawEllipse, GdipDrawImageRect, GdipDrawLines, GdipFillPath,
    GdipFillRectangleI, GdipSetInterpolationMode, GdipSetPenLineCap197819, GdipSetPenLineJoin,
    GdipSetSmoothingMode, GdipSetSolidFillColor, GpGraphics, GpSolidFill,
    InterpolationModeHighQualityBicubic, LineCapRound, LineJoinRound, Ok as GpOk, PointF,
    SmoothingModeAntiAlias, UnitPixel,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

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
        let mut use_light = 1u32;
        let mut length = size_of::<u32>() as u32;
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut use_light as *mut u32).cast()),
                Some(&mut length),
            )
        };
        if result.is_ok() && use_light == 0 {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

pub(super) struct PopupFonts {
    body: HFONT,
    secondary: HFONT,
    heading: HFONT,
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
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                FF_DONTCARE.0 as u32,
                w!("Segoe UI Variable Text"),
            )
        };
        Self {
            body: create(14, FW_NORMAL.0 as i32),
            secondary: create(12, FW_NORMAL.0 as i32),
            heading: create(12, FW_SEMIBOLD.0 as i32),
        }
    }
}

impl Drop for PopupFonts {
    fn drop(&mut self) {
        for font in [self.body, self.secondary, self.heading] {
            if !font.is_invalid() {
                let _ = unsafe { DeleteObject(font) };
            }
        }
    }
}

impl CursorPopup {
    pub(super) unsafe fn paint(&mut self, hdc: HDC) {
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        let dc = CreateCompatibleDC(hdc);
        let bitmap = CreateCompatibleBitmap(hdc, self.width, self.height);
        let buffered = !dc.is_invalid() && !bitmap.is_invalid();
        let target = if buffered { dc } else { hdc };
        let old_bitmap = if buffered {
            SelectObject(dc, bitmap)
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
            let _ = BitBlt(hdc, 0, 0, self.width, self.height, dc, 0, 0, SRCCOPY);
            let _ = SelectObject(dc, old_bitmap);
        }
        let _ = DeleteObject(bitmap);
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

        let icon_size = px(16);
        for (index, scene) in self.scenes.iter().enumerate() {
            let Some(rect) = self.viewport_rect(self.layout.scene(index, self.width)) else {
                continue;
            };
            let active = self.active_scene_id.as_deref() == Some(scene.id.as_str());
            let radius = px(6);
            if active {
                canvas.fill(rect, radius, palette.active_border);
                canvas.fill(inset(rect, border), radius - border, palette.active);
            } else {
                let fill = if self.hover == Some(Hit::Scene(index)) {
                    palette.hover
                } else {
                    palette.tile
                };
                canvas.fill(rect, radius, fill);
            }

            let x = rect.left + (rect.width() - icon_size) / 2;
            let y = rect.top + px(9);
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
                let icon = match scene.icon.as_str() {
                    "chart" => Icon::Chart,
                    "code" => Icon::Code,
                    "globe" => Icon::Globe,
                    _ => Icon::Circle,
                };
                let color = if active { palette.accent } else { palette.text };
                canvas.icon(icon, x, y, icon_size, color);
            }
        }

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
            if self.hover == Some(hit) {
                canvas.fill(rect, px(5), palette.hover);
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
        let old_font = select_font(hdc, self.fonts.heading);
        let heading = |text, top| {
            if let Some(rect) = self.viewport_rect(RectData {
                left: self.layout.pad,
                right: self.width - self.layout.pad,
                top,
                bottom: top + px(17),
            }) {
                draw_text(hdc, text, rect, DT_LEFT, palette.muted);
            }
        };
        if !self.scenes.is_empty() {
            heading("シーン", self.layout.scene_header_top);
        }
        heading("ショートカット", self.layout.shortcut_header_top);

        for (index, scene) in self.scenes.iter().enumerate() {
            let Some(rect) = self.viewport_rect(self.layout.scene(index, self.width)) else {
                continue;
            };
            let active = self.active_scene_id.as_deref() == Some(scene.id.as_str());
            let color = if active { palette.accent } else { palette.text };
            select_font(hdc, self.fonts.body);
            draw_text(
                hdc,
                &scene.name,
                RectData {
                    left: rect.left + px(3),
                    right: rect.right - px(3),
                    top: rect.top + px(29),
                    bottom: rect.top + px(49),
                },
                DT_CENTER,
                color,
            );
            if active {
                select_font(hdc, self.fonts.secondary);
                draw_text(
                    hdc,
                    "✓",
                    RectData {
                        right: rect.right - px(6),
                        top: rect.top + px(3),
                        bottom: rect.top + px(20),
                        ..rect
                    },
                    DT_RIGHT,
                    color,
                );
            }
        }

        for index in 0..self.shortcuts.len().max(1) {
            let Some(rect) = self.viewport_rect(self.layout.shortcut(index, self.width)) else {
                continue;
            };
            if let Some(shortcut) = self.shortcuts.get(index) {
                select_font(hdc, self.fonts.secondary);
                let right = rect.right - px(6);
                let kind_width = text_width(hdc, shortcut.kind.label());
                let kind = RectData {
                    left: right - kind_width,
                    right,
                    ..rect
                };
                draw_text(hdc, shortcut.kind.label(), kind, DT_RIGHT, palette.muted);

                select_font(hdc, self.fonts.body);
                let name = RectData {
                    left: rect.left + px(32),
                    right: kind.left - px(8),
                    ..rect
                };
                draw_text(hdc, &shortcut.name, name, DT_LEFT, palette.text);
            } else {
                select_font(hdc, self.fonts.body);
                draw_text(
                    hdc,
                    "ショートカットを追加…",
                    RectData {
                        left: rect.left + px(6),
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

unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    let text: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &text, &mut size);
    size.cx
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
        let ellipse = |left: f32, top: f32, width: f32, height: f32| {
            let _ = GdipDrawEllipse(
                self.graphics,
                pen,
                x as f32 + left * scale,
                y as f32 + top * scale,
                width * scale,
                height * scale,
            );
        };
        match icon {
            Icon::Chart => {
                line(&[(2.0, 15.0), (9.0, 8.0), (13.0, 12.0), (22.0, 3.0)]);
                for (x, top) in [
                    (4.0, 18.0),
                    (8.0, 14.0),
                    (12.0, 16.0),
                    (16.0, 14.0),
                    (20.0, 10.0),
                ] {
                    line(&[(x, top), (x, 21.0)]);
                }
            }
            Icon::Code => {
                line(&[(8.0, 6.0), (2.0, 12.0), (8.0, 18.0)]);
                line(&[(16.0, 6.0), (22.0, 12.0), (16.0, 18.0)]);
                line(&[(14.0, 4.0), (10.0, 20.0)]);
            }
            Icon::Globe => {
                ellipse(2.0, 2.0, 20.0, 20.0);
                ellipse(8.0, 2.0, 8.0, 20.0);
                line(&[(2.0, 12.0), (22.0, 12.0)]);
            }
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
            Icon::Circle => ellipse(4.0, 4.0, 16.0, 16.0),
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
    Chart,
    Code,
    Globe,
    Folder,
    File,
    Website,
    Circle,
}
