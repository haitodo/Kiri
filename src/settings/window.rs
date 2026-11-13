//! Win32 Settings Window implementation.
//!
//! Provides an on-demand dialog window with five tabs:
//! 1. Scenes: Add, remove, rename, order, icon, and pinned status
//! 2. Windows: Enumerate open candidate windows, warnings, and scene binding
//! 3. Launcher: Orientation, button/grip size, visible slots, topmost, snap, focus
//! 4. Behavior: Target monitor selector, Windows autostart, verbose logging, full restore
//! 5. Shortcuts: Folders, files, and websites opened by the cursor launcher

use crate::app::AppController;
use crate::config::{CursorPopupPosition, MonitorPolicy, Orientation, Scene, Shortcut, ShortcutKind};
use crate::settings::{set_autostart_enabled, SettingsTab, SettingsViewModel};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(windows)]
use windows::Win32::UI::Controls::{
    LVN_ITEMCHANGED, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_FULLROWSELECT, LVS_EX_GRIDLINES,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMITEMACTIVATE, NMLISTVIEW, WC_LISTVIEWW,
};

pub const WM_APP_SETTINGS_SAVED: u32 = 0x8000 + 5;
const WM_APP_SHOW_SETTINGS_TAB: u32 = 0x8000 + 10;

static ACTIVE_SETTINGS_HWND: AtomicUsize = AtomicUsize::new(0);

const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

const NM_CLICK: u32 = 0u32.wrapping_sub(2); // 0xFFFFFFFE
const NM_DBLCLK: u32 = 0u32.wrapping_sub(3); // 0xFFFFFFFD
const EN_CHANGE: u32 = 0x0300;
const WM_SETICON: u32 = 0x0080;

// Control IDs
const IDC_TAB_CONTROL: usize = 100;
const IDC_BTN_OK: usize = 101;
const IDC_BTN_CANCEL: usize = 102;
const IDC_BTN_APPLY: usize = 103;

// Tab 0 (Scenes)
const IDC_SCENE_LIST: usize = 201;
const IDC_SCENE_ADD: usize = 202;
const IDC_SCENE_DEL: usize = 203;
const IDC_SCENE_UP: usize = 204;
const IDC_SCENE_DOWN: usize = 205;
const IDC_SCENE_ID: usize = 206;
const IDC_SCENE_NAME: usize = 207;
const IDC_SCENE_ICON: usize = 208;
const IDC_SCENE_PINNED: usize = 209;
const IDC_SCENE_UPDATE: usize = 210;
const IDC_SCENE_APPS_LIST: usize = 211;
const IDC_SCENE_APP_UNBIND: usize = 212;
const IDC_SCENE_ICON_PRESET: usize = 213;
const IDC_SCENE_ICON_BROWSE: usize = 214;
const IDC_SCENE_LAYOUT_STATUS: usize = 215;
const IDC_SCENE_LAYOUT_DELETE: usize = 216;
const IDC_SCENE_HK_CTRL: usize = 217;
const IDC_SCENE_HK_ALT: usize = 218;
const IDC_SCENE_HK_SHIFT: usize = 219;
const IDC_SCENE_HK_WIN: usize = 220;
const IDC_SCENE_HK_KEY: usize = 221;
const IDC_SCENE_HK_CLEAR: usize = 222;
const IDC_SCENE_HK_WARN: usize = 223;

// Tab 1 (Apps & Scenes Matrix)
const IDC_APP_MATRIX_LIST: usize = 301;
const IDC_APP_ADD_WINDOW: usize = 302;
const IDC_APP_DELETE: usize = 303;
const IDC_APP_REFRESH: usize = 304;
const IDC_APP_TITLE_KEYWORD: usize = 305;
const IDC_APP_MATCH_MONITOR: usize = 306;
const IDC_APP_APPLY_RULE: usize = 307;
const IDC_APP_DETAILS: usize = 308;
const IDC_APP_RESOLVE: usize = 309;

// Tab 2 (Launcher)
const IDC_LCH_ORIENT_COMBO: usize = 401;
const IDC_LCH_BTN_SIZE: usize = 402;
const IDC_LCH_GRIP_SIZE: usize = 403;
const IDC_LCH_SLOTS: usize = 404;
const IDC_LCH_TOPMOST: usize = 405;
const IDC_LCH_SNAP: usize = 406;
const IDC_LCH_FOCUS: usize = 407;
const IDC_LCH_RESET_POS: usize = 408;
const IDC_LCH_SHOW_BAR: usize = 409;
const IDC_LCH_CTRL_DOUBLE_TAP: usize = 410;
const IDC_LCH_POPUP_POSITION: usize = 411;
const IDC_LCH_WHEEL_WRAP: usize = 412;

const CURSOR_POPUP_POSITION_CHOICES: [(CursorPopupPosition, windows::core::PCWSTR); 2] = [
    (
        CursorPopupPosition::SceneCentered,
        windows::core::w!("シーンをカーソルに寄せる"),
    ),
    (
        CursorPopupPosition::BesideCursor,
        windows::core::w!("カーソルの斜め横に表示"),
    ),
];

// Tab 3 (Behavior)
const IDC_BEH_MONITOR_COMBO: usize = 501;
const IDC_BEH_AUTOSTART: usize = 502;
const IDC_BEH_VERBOSE_TITLE: usize = 503;
const IDC_BEH_RESTORE_ALL: usize = 504;
const IDC_BEH_OPEN_FOLDER: usize = 505;
const IDC_BEH_RESTORE_ALL_ON_SWITCH: usize = 506;
const IDC_BEH_EXPORT: usize = 507;
const IDC_BEH_IMPORT: usize = 508;

// Add Window Subdialog IDs
const IDC_ADDWIN_LIST: usize = 601;
const IDC_ADDWIN_SCENE_COMBO: usize = 602;
const IDC_ADDWIN_ALWAYS_VISIBLE: usize = 603;
const IDC_ADDWIN_PIN_MONITOR: usize = 604;
const IDC_ADDWIN_KEYWORD: usize = 605;
const IDC_ADDWIN_BTN_ADD: usize = 606;
const IDC_ADDWIN_BTN_CLOSE: usize = 607;
const IDC_ADDWIN_STATUS: usize = 608;

// Pick Candidate Subdialog IDs
const IDC_PICKCAND_LIST: usize = 651;
const IDC_PICKCAND_BTN_OK: usize = 652;
const IDC_PICKCAND_BTN_CANCEL: usize = 653;

// Win32 Hotkey virtual-key options `(vk, label)`
const HOTKEY_KEY_CHOICES: &[(u32, &str)] = &[
    (0, "(未設定)"),
    (0x70, "F1"),
    (0x71, "F2"),
    (0x72, "F3"),
    (0x73, "F4"),
    (0x74, "F5"),
    (0x75, "F6"),
    (0x76, "F7"),
    (0x77, "F8"),
    (0x78, "F9"),
    (0x79, "F10"),
    (0x7A, "F11"),
    (0x7B, "F12"),
    (0x31, "1"),
    (0x32, "2"),
    (0x33, "3"),
    (0x34, "4"),
    (0x35, "5"),
    (0x36, "6"),
    (0x37, "7"),
    (0x38, "8"),
    (0x39, "9"),
    (0x30, "0"),
    (0x41, "A"),
    (0x42, "B"),
    (0x43, "C"),
    (0x44, "D"),
    (0x45, "E"),
    (0x46, "F"),
    (0x47, "G"),
    (0x48, "H"),
    (0x49, "I"),
    (0x4A, "J"),
    (0x4B, "K"),
    (0x4C, "L"),
    (0x4D, "M"),
    (0x4E, "N"),
    (0x4F, "O"),
    (0x50, "P"),
    (0x51, "Q"),
    (0x52, "R"),
    (0x53, "S"),
    (0x54, "T"),
    (0x55, "U"),
    (0x56, "V"),
    (0x57, "W"),
    (0x58, "X"),
    (0x59, "Y"),
    (0x5A, "Z"),
];

// Tab 4 (Shortcuts)
const IDC_SHORTCUT_LIST: usize = 701;
const IDC_SHORTCUT_NEW: usize = 702;
const IDC_SHORTCUT_DELETE: usize = 703;
const IDC_SHORTCUT_UP: usize = 704;
const IDC_SHORTCUT_DOWN: usize = 705;
const IDC_SHORTCUT_NAME: usize = 706;
const IDC_SHORTCUT_KIND: usize = 707;
const IDC_SHORTCUT_TARGET: usize = 708;
const IDC_SHORTCUT_BROWSE: usize = 709;
const IDC_SHORTCUT_UPDATE: usize = 710;

const SETTINGS_CLIENT_W_DIP: i32 = 820;
const SETTINGS_CLIENT_H_DIP: i32 = 560;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Standard,
    ListBox,
    ComboBox,
    TabControl,
    ListView,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
pub struct ControlDipLayout {
    pub hwnd: windows::Win32::Foundation::HWND,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: ControlKind,
}

#[cfg(windows)]
pub struct SettingsWindow {
    pub hwnd: windows::Win32::Foundation::HWND,
    pub parent_hwnd: windows::Win32::Foundation::HWND,
    pub vm: SettingsViewModel,
    pub selected_scene_idx: Option<usize>,
    pub selected_app_idx: Option<usize>,
    selected_shortcut_idx: Option<usize>,
    pub tab_controls: [Vec<windows::Win32::Foundation::HWND>; 5],
    pub control_layouts: Vec<ControlDipLayout>,
    pub hfont: windows::Win32::Graphics::Gdi::HFONT,
    pub icon_small: windows::Win32::UI::WindowsAndMessaging::HICON,
    pub icon_big: windows::Win32::UI::WindowsAndMessaging::HICON,
    pub dpi: u32,
    pub applying_dpi: bool,
    pub updating_fields: bool,
    config_path: std::path::PathBuf,
}

#[cfg(windows)]
impl Drop for SettingsWindow {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
        unsafe {
            if !self.hfont.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.hfont.0));
            }
            if !self.icon_small.0.is_null() {
                let _ = DestroyIcon(self.icon_small);
            }
            if !self.icon_big.0.is_null() {
                let _ = DestroyIcon(self.icon_big);
            }
        }
    }
}

#[cfg(windows)]
unsafe fn create_settings_ui_font(dpi: u32) -> windows::Win32::Graphics::Gdi::HFONT {
    use crate::platform::monitors::dip_to_px;
    use windows::core::w;
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_DONTCARE,
        FW_NORMAL, OUT_DEFAULT_PRECIS,
    };
    let height = -dip_to_px(14, dpi.max(72));
    CreateFontW(
        height,
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
        w!("Segoe UI Variable Text"),
    )
}

#[cfg(windows)]
fn compute_settings_outer_size(dpi: u32) -> (i32, i32) {
    use crate::platform::monitors::dip_to_px;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
    use windows::Win32::UI::WindowsAndMessaging::{
        WINDOW_EX_STYLE, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    };

    let d = dpi.max(72);
    let client_w = dip_to_px(SETTINGS_CLIENT_W_DIP, d);
    let client_h = dip_to_px(SETTINGS_CLIENT_H_DIP, d);
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WINDOW_EX_STYLE(0), d);
    }
    (rc.right - rc.left, rc.bottom - rc.top)
}

#[cfg(windows)]
fn compute_effective_dpi(dpi: u32, work_area: Option<crate::config::RectData>) -> u32 {
    let mut eff = dpi.max(96);
    if let Some(wa) = work_area {
        let (outer_w, outer_h) = compute_settings_outer_size(eff);
        let avail_w = wa.width().max(400);
        let avail_h = wa.height().max(300);
        if outer_w > avail_w || outer_h > avail_h {
            let ratio_w = avail_w as f64 / outer_w.max(1) as f64;
            let ratio_h = avail_h as f64 / outer_h.max(1) as f64;
            let scale = ratio_w.min(ratio_h).clamp(0.5, 1.0);
            eff = ((eff as f64 * scale).floor() as u32).max(72);
        }
    }
    eff
}

fn dlg_item(hwnd: windows::Win32::Foundation::HWND, id: usize) -> windows::Win32::Foundation::HWND {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgItem(hwnd, id as i32).unwrap_or_default() }
}

#[cfg(windows)]
impl SettingsWindow {
    pub fn show(parent_hwnd: windows::Win32::Foundation::HWND, app: &AppController, tab: Option<SettingsTab>) {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::Controls::{
            InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
            INITCOMMONCONTROLSEX,
        };
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, IsIconic, LoadCursorW, PostMessageW, RegisterClassW, SetForegroundWindow,
            SetWindowLongPtrW, ShowWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW,
            SW_RESTORE, SW_SHOW, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
        };

        // If settings window is already open, activate and bring it to front
        let existing = ACTIVE_SETTINGS_HWND.load(Ordering::Acquire);
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
                if let Some(tab) = tab {
                    let _ = PostMessageW(hwnd, WM_APP_SHOW_SETTINGS_TAB, WPARAM(tab as usize), LPARAM(0));
                }
            }
            return;
        }

        static REGISTER_ONCE: Once = Once::new();
        let class_name = w!("KiriSettingsWindowClass");

        unsafe {
            let icex = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
            };
            let _ = InitCommonControlsEx(&icex);

            let hinstance = GetModuleHandleW(None).unwrap();
            REGISTER_ONCE.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(settings_wndproc),
                    hInstance: hinstance.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut _),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let launcher_mon = if app.launcher_monitor_id.is_empty() {
                None
            } else {
                Some(app.launcher_monitor_id.as_str())
            };
            let vm = SettingsViewModel::build(&app.config, &*app.sys, launcher_mon);

            let init_mon = launcher_mon
                .and_then(|id| vm.monitors.iter().find(|m| m.monitor_id == id))
                .or_else(|| vm.monitors.iter().find(|m| m.is_primary))
                .or_else(|| vm.monitors.first());

            let (init_x, init_y, outer_w, outer_h, init_dpi) = if let Some(mon) = init_mon {
                let eff_dpi = compute_effective_dpi(mon.dpi, Some(mon.work_area));
                let (w, h) = compute_settings_outer_size(eff_dpi);
                let x = mon.work_area.left + ((mon.work_area.width() - w) / 2).max(0);
                let y = mon.work_area.top + ((mon.work_area.height() - h) / 2).max(0);
                (x, y, w, h, eff_dpi)
            } else {
                let (w, h) = compute_settings_outer_size(96);
                (100, 100, w, h, 96)
            };

            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                class_name,
                w!("Kiri 設定"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                init_x,
                init_y,
                outer_w,
                outer_h,
                parent_hwnd,
                None,
                hinstance,
                None,
            )
            .unwrap();

            ACTIVE_SETTINGS_HWND.store(hwnd.0 as usize, Ordering::Release);

            let actual_win_dpi = {
                let d = GetDpiForWindow(hwnd);
                if d > 0 { d } else { init_dpi }
            };

            let icon_small = crate::launcher::paint::create_kiri_hicon(16).unwrap_or_default();
            let icon_big = crate::launcher::paint::create_kiri_hicon(32).unwrap_or_default();
            if !icon_small.0.is_null() {
                let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(0),
                    LPARAM(icon_small.0 as isize),
                );
            }
            if !icon_big.0.is_null() {
                let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(1),
                    LPARAM(icon_big.0 as isize),
                );
            }

            let mut win = Box::new(Self {
                hwnd,
                parent_hwnd,
                vm,
                selected_scene_idx: None,
                selected_app_idx: None,
                selected_shortcut_idx: None,
                tab_controls: std::array::from_fn(|_| Vec::new()),
                control_layouts: Vec::with_capacity(64),
                hfont: windows::Win32::Graphics::Gdi::HFONT::default(),
                icon_small,
                icon_big,
                dpi: actual_win_dpi,
                applying_dpi: false,
                updating_fields: false,
                config_path: app.config_mgr.config_path().to_path_buf(),
            });

            let ptr = &mut *win as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            win.init_controls(hinstance, tab.unwrap_or(SettingsTab::Scenes));
            win.clamp_to_current_monitor_work_area();

            // Box is intentionally leaked while the window is alive; dropped in WM_NCDESTROY
            let _ = Box::into_raw(win);

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    unsafe fn init_controls(&mut self, hinstance: windows::Win32::Foundation::HMODULE, tab: SettingsTab) {
        use crate::platform::monitors::dip_to_px;
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::Controls::{TCIF_TEXT, TCITEMW, TCM_INSERTITEMW, WC_TABCONTROLW};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, SendMessageW, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST,
            CB_ADDSTRING, CB_SETCURSEL, ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY,
            LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, WS_BORDER, WS_CHILD, WS_HSCROLL, WS_VISIBLE,
            WS_VSCROLL,
        };

        let dpi = self.dpi.max(96);
        self.hfont = create_settings_ui_font(dpi);
        let font = self.hfont;

        let mut layouts = Vec::with_capacity(48);

        // Helper closure to create, font, and register a control with DIP coordinates
        let mut create_ctrl = |class: PCWSTR,
                               text: PCWSTR,
                               style_u32: u32,
                               x_dip: i32,
                               y_dip: i32,
                               w_dip: i32,
                               h_dip: i32,
                               id: usize,
                               kind: ControlKind|
         -> HWND {
            let hctrl = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                class,
                text,
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    style_u32 | WS_CHILD.0 | WS_VISIBLE.0,
                ),
                dip_to_px(x_dip, dpi),
                dip_to_px(y_dip, dpi),
                dip_to_px(w_dip, dpi),
                dip_to_px(h_dip, dpi),
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _),
                hinstance,
                None,
            )
            .unwrap();
            let _ = SendMessageW(hctrl, 0x0030 /* WM_SETFONT */, WPARAM(font.0 as usize), LPARAM(1));
            layouts.push(ControlDipLayout {
                hwnd: hctrl,
                x: x_dip,
                y: y_dip,
                w: w_dip,
                h: h_dip,
                kind,
            });
            hctrl
        };

        // Tab Control
        let tab_hwnd = create_ctrl(
            WC_TABCONTROLW,
            w!(""),
            0,
            12,
            12,
            796,
            492,
            IDC_TAB_CONTROL,
            ControlKind::TabControl,
        );

        let tab_titles = [w!("シーン"), w!("アプリとシーン"), w!("ランチャー"), w!("動作"), w!("ショートカット")];
        for (i, title) in tab_titles.iter().enumerate() {
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: windows::core::PWSTR(title.as_ptr() as *mut u16),
                ..Default::default()
            };
            let _ = SendMessageW(
                tab_hwnd,
                TCM_INSERTITEMW,
                WPARAM(i),
                LPARAM(&mut item as *mut _ as isize),
            );
        }

        // Bottom dialog buttons
        let _ = create_ctrl(
            w!("BUTTON"),
            w!("OK"),
            BS_PUSHBUTTON as u32,
            500,
            514,
            96,
            34,
            IDC_BTN_OK,
            ControlKind::Standard,
        );
        let _ = create_ctrl(
            w!("BUTTON"),
            w!("キャンセル"),
            BS_PUSHBUTTON as u32,
            606,
            514,
            100,
            34,
            IDC_BTN_CANCEL,
            ControlKind::Standard,
        );
        let _ = create_ctrl(
            w!("BUTTON"),
            w!("適用"),
            BS_PUSHBUTTON as u32,
            712,
            514,
            96,
            34,
            IDC_BTN_APPLY,
            ControlKind::Standard,
        );

        // ===================== Tab 0: Scenes =====================
        let mut t0 = Vec::new();
        t0.push(create_ctrl(
            w!("STATIC"),
            w!("登録済みシーン一覧:"),
            0,
            28,
            48,
            256,
            22,
            0,
            ControlKind::Standard,
        ));
        let scene_list = create_ctrl(
            w!("LISTBOX"),
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (LBS_NOTIFY as u32) | (LBS_NOINTEGRALHEIGHT as u32),
            28,
            74,
            256,
            358,
            IDC_SCENE_LIST,
            ControlKind::ListBox,
        );
        t0.push(scene_list);

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("新規作成"),
            BS_PUSHBUTTON as u32,
            28,
            442,
            84,
            32,
            IDC_SCENE_ADD,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("削除"),
            BS_PUSHBUTTON as u32,
            116,
            442,
            70,
            32,
            IDC_SCENE_DEL,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("↑"),
            BS_PUSHBUTTON as u32,
            192,
            442,
            44,
            32,
            IDC_SCENE_UP,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("↓"),
            BS_PUSHBUTTON as u32,
            240,
            442,
            44,
            32,
            IDC_SCENE_DOWN,
            ControlKind::Standard,
        ));

        // Right details area (x: 302..792)
        t0.push(create_ctrl(
            w!("STATIC"),
            w!("シーンID:"),
            0,
            302,
            48,
            120,
            18,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_READONLY as u32),
            302,
            68,
            120,
            26,
            IDC_SCENE_ID,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("表示名:"),
            0,
            430,
            48,
            200,
            18,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            430,
            68,
            230,
            26,
            IDC_SCENE_NAME,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("一覧へ反映"),
            BS_PUSHBUTTON as u32,
            668,
            68,
            122,
            26,
            IDC_SCENE_UPDATE,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("アイコン (プリセット選択または画像ファイル参照):"),
            0,
            302,
            100,
            480,
            18,
            0,
            ControlKind::Standard,
        ));
        let icon_preset_combo = create_ctrl(
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            302,
            120,
            185,
            190,
            IDC_SCENE_ICON_PRESET,
            ControlKind::ComboBox,
        );
        for (id, label) in crate::launcher::paint::BUILTIN_ICONS {
            let entry: Vec<u16> = format!("{} — {}", id, label)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let _ = SendMessageW(icon_preset_combo, CB_ADDSTRING, WPARAM(0), LPARAM(entry.as_ptr() as isize));
        }
        let custom_entry: Vec<u16> = "カスタム (.ico / .png)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let _ = SendMessageW(
            icon_preset_combo,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(custom_entry.as_ptr() as isize),
        );
        t0.push(icon_preset_combo);

        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            493,
            120,
            215,
            26,
            IDC_SCENE_ICON,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("参照…"),
            BS_PUSHBUTTON as u32,
            714,
            120,
            76,
            26,
            IDC_SCENE_ICON_BROWSE,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("固定バーに常時表示する (ピン留め)"),
            BS_AUTOCHECKBOX as u32,
            302,
            152,
            340,
            24,
            IDC_SCENE_PINNED,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("保存レイアウト: なし"),
            0,
            302,
            182,
            348,
            22,
            IDC_SCENE_LAYOUT_STATUS,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("レイアウト削除"),
            BS_PUSHBUTTON as u32,
            658,
            179,
            132,
            26,
            IDC_SCENE_LAYOUT_DELETE,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("グローバルホットキー:"),
            0,
            302,
            212,
            148,
            20,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("Ctrl"),
            BS_AUTOCHECKBOX as u32,
            454,
            210,
            54,
            24,
            IDC_SCENE_HK_CTRL,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("Alt"),
            BS_AUTOCHECKBOX as u32,
            510,
            210,
            50,
            24,
            IDC_SCENE_HK_ALT,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("Shift"),
            BS_AUTOCHECKBOX as u32,
            562,
            210,
            60,
            24,
            IDC_SCENE_HK_SHIFT,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("Win"),
            BS_AUTOCHECKBOX as u32,
            624,
            210,
            54,
            24,
            IDC_SCENE_HK_WIN,
            ControlKind::Standard,
        ));

        let hk_key_combo = create_ctrl(
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            302,
            236,
            160,
            200,
            IDC_SCENE_HK_KEY,
            ControlKind::ComboBox,
        );
        for &(_, label) in HOTKEY_KEY_CHOICES {
            let entry: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(hk_key_combo, CB_ADDSTRING, WPARAM(0), LPARAM(entry.as_ptr() as isize));
        }
        t0.push(hk_key_combo);

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("クリア"),
            BS_PUSHBUTTON as u32,
            468,
            236,
            68,
            26,
            IDC_SCENE_HK_CLEAR,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("STATIC"),
            w!(""),
            0,
            542,
            239,
            248,
            20,
            IDC_SCENE_HK_WARN,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("このシーンに登録されているアプリ:"),
            0,
            302,
            268,
            480,
            20,
            0,
            ControlKind::Standard,
        ));
        let scene_apps_list = create_ctrl(
            w!("LISTBOX"),
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (LBS_NOTIFY as u32) | (LBS_NOINTEGRALHEIGHT as u32),
            302,
            290,
            488,
            144,
            IDC_SCENE_APPS_LIST,
            ControlKind::ListBox,
        );
        t0.push(scene_apps_list);

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("このシーンから外す"),
            BS_PUSHBUTTON as u32,
            618,
            442,
            172,
            32,
            IDC_SCENE_APP_UNBIND,
            ControlKind::Standard,
        ));
        self.tab_controls[0] = t0;

        // ===================== Tab 1: Apps & Scenes Matrix =====================
        let mut t1 = Vec::new();
        t1.push(create_ctrl(
            w!("STATIC"),
            w!("登録アプリとシーン対応表 (セルクリックで ☑/☐ 切替):"),
            0,
            28,
            48,
            380,
            24,
            0,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("＋ ウィンドウから追加…"),
            BS_PUSHBUTTON as u32,
            418,
            44,
            186,
            30,
            IDC_APP_ADD_WINDOW,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("登録解除"),
            BS_PUSHBUTTON as u32,
            614,
            44,
            90,
            30,
            IDC_APP_DELETE,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("一覧更新"),
            BS_PUSHBUTTON as u32,
            712,
            44,
            80,
            30,
            IDC_APP_REFRESH,
            ControlKind::Standard,
        ));

        let matrix_list = create_ctrl(
            WC_LISTVIEWW,
            w!(""),
            WS_BORDER.0
                | WS_VSCROLL.0
                | WS_HSCROLL.0
                | (LVS_REPORT as u32)
                | (LVS_SINGLESEL as u32)
                | (LVS_SHOWSELALWAYS as u32),
            28,
            80,
            764,
            256,
            IDC_APP_MATRIX_LIST,
            ControlKind::ListView,
        );
        let _ = SendMessageW(
            matrix_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize),
        );
        t1.push(matrix_list);

        t1.push(create_ctrl(
            w!("STATIC"),
            w!("再起動時の識別キーワード (空欄=タイトル不問):"),
            0,
            28,
            344,
            320,
            20,
            0,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            28,
            366,
            320,
            28,
            IDC_APP_TITLE_KEYWORD,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("配置モニター(左/右)で識別する"),
            BS_AUTOCHECKBOX as u32,
            360,
            366,
            260,
            28,
            IDC_APP_MATCH_MONITOR,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("条件を更新"),
            BS_PUSHBUTTON as u32,
            630,
            366,
            162,
            28,
            IDC_APP_APPLY_RULE,
            ControlKind::Standard,
        ));

        t1.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_MULTILINE as u32) | (ES_READONLY as u32) | WS_VSCROLL.0,
            28,
            402,
            592,
            76,
            IDC_APP_DETAILS,
            ControlKind::Standard,
        ));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("候補を特定…"),
            BS_PUSHBUTTON as u32,
            630,
            402,
            162,
            30,
            IDC_APP_RESOLVE,
            ControlKind::Standard,
        ));
        self.tab_controls[1] = t1;

        // ===================== Tab 2: Launcher =====================
        let mut t2 = Vec::new();
        t2.push(create_ctrl(
            w!("STATIC"),
            w!("配置方向:"),
            0,
            32,
            64,
            210,
            24,
            0,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST as u32,
            250,
            60,
            190,
            140,
            IDC_LCH_ORIENT_COMBO,
            ControlKind::ComboBox,
        ));

        t2.push(create_ctrl(
            w!("STATIC"),
            w!("ボタンサイズ (DIP):"),
            0,
            32,
            106,
            210,
            24,
            0,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("EDIT"),
            w!("32"),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            250,
            102,
            96,
            28,
            IDC_LCH_BTN_SIZE,
            ControlKind::Standard,
        ));

        t2.push(create_ctrl(
            w!("STATIC"),
            w!("グリップサイズ (DIP):"),
            0,
            32,
            148,
            210,
            24,
            0,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("EDIT"),
            w!("18"),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            250,
            144,
            96,
            28,
            IDC_LCH_GRIP_SIZE,
            ControlKind::Standard,
        ));

        t2.push(create_ctrl(
            w!("STATIC"),
            w!("常時表示件数 (1〜12):"),
            0,
            32,
            190,
            210,
            24,
            0,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("EDIT"),
            w!("3"),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            250,
            186,
            96,
            28,
            IDC_LCH_SLOTS,
            ControlKind::Standard,
        ));

        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("常時最前面に表示する"),
            BS_AUTOCHECKBOX as u32,
            32,
            234,
            480,
            28,
            IDC_LCH_TOPMOST,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("画面端にスナップする (12 DIP以内)"),
            BS_AUTOCHECKBOX as u32,
            32,
            266,
            520,
            28,
            IDC_LCH_SNAP,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("シーン切替時に最前面ウィンドウへフォーカスを移動する"),
            BS_AUTOCHECKBOX as u32,
            32,
            298,
            660,
            28,
            IDC_LCH_FOCUS,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("ホイールでシーンを一巡する"),
            BS_AUTOCHECKBOX as u32,
            32,
            330,
            440,
            28,
            IDC_LCH_WHEEL_WRAP,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("ランチャーの位置を初期化"),
            BS_PUSHBUTTON as u32,
            32,
            362,
            220,
            34,
            IDC_LCH_RESET_POS,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"), w!("固定バーを表示する"), BS_AUTOCHECKBOX as u32,
            32, 407, 440, 28, IDC_LCH_SHOW_BAR, ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"), w!("Ctrl の二度押しでカーソル付近にランチャーを開く"), BS_AUTOCHECKBOX as u32,
            32, 438, 550, 28, IDC_LCH_CTRL_DOUBLE_TAP, ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("STATIC"), w!("ポップアップの表示位置"), 0,
            32, 475, 210, 24, 0, ControlKind::Standard,
        ));
        let popup_position = create_ctrl(
            w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32,
            250, 471, 260, 100, IDC_LCH_POPUP_POSITION, ControlKind::ComboBox,
        );
        for (index, (position, label)) in CURSOR_POPUP_POSITION_CHOICES.into_iter().enumerate() {
            let _ = SendMessageW(
                popup_position,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
            if position == self.vm.config_draft.launcher.cursor_popup_position {
                let _ = SendMessageW(popup_position, CB_SETCURSEL, WPARAM(index), LPARAM(0));
            }
        }
        t2.push(popup_position);
        self.tab_controls[2] = t2;

        // ===================== Tab 3: Behavior =====================
        let mut t3 = Vec::new();
        t3.push(create_ctrl(
            w!("STATIC"),
            w!("制御対象モニター:"),
            0,
            32,
            64,
            175,
            24,
            0,
            ControlKind::Standard,
        ));
        t3.push(create_ctrl(
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST as u32,
            212,
            60,
            575,
            220,
            IDC_BEH_MONITOR_COMBO,
            ControlKind::ComboBox,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("シーン切替時に登録アプリを確実にすべて表示する (手動最小化されたアプリも復元)"),
            BS_AUTOCHECKBOX as u32,
            32,
            110,
            740,
            28,
            IDC_BEH_RESTORE_ALL_ON_SWITCH,
            ControlKind::Standard,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("Windows ログオン時に自動起動する"),
            BS_AUTOCHECKBOX as u32,
            32,
            148,
            560,
            28,
            IDC_BEH_AUTOSTART,
            ControlKind::Standard,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("ログにウィンドウタイトル全文を記録する (口座番号等のマスクを解除)"),
            BS_AUTOCHECKBOX as u32,
            32,
            186,
            680,
            28,
            IDC_BEH_VERBOSE_TITLE,
            ControlKind::Standard,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("Kiri が最小化した全ウィンドウを復元"),
            BS_PUSHBUTTON as u32,
            32,
            240,
            320,
            36,
            IDC_BEH_RESTORE_ALL,
            ControlKind::Standard,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("設定保存先フォルダを開く"),
            BS_PUSHBUTTON as u32,
            32,
            290,
            230,
            34,
            IDC_BEH_OPEN_FOLDER,
            ControlKind::Standard,
        ));
        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("設定をエクスポート…"),
            BS_PUSHBUTTON as u32,
            274,
            290,
            200,
            34,
            IDC_BEH_EXPORT,
            ControlKind::Standard,
        ));
        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("設定をインポート…"),
            BS_PUSHBUTTON as u32,
            486,
            290,
            200,
            34,
            IDC_BEH_IMPORT,
            ControlKind::Standard,
        ));
        self.tab_controls[3] = t3;

        let shortcut_kind = create_ctrl(
            w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32,
            330, 160, 180, 140, IDC_SHORTCUT_KIND, ControlKind::ComboBox,
        );
        self.tab_controls[4] = vec![
            create_ctrl(w!("STATIC"), w!("登録済みショートカット"), 0, 28, 50, 280, 22, 0, ControlKind::Standard),
            create_ctrl(w!("LISTBOX"), w!(""),
                WS_BORDER.0 | WS_VSCROLL.0 | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
                28, 76, 280, 330, IDC_SHORTCUT_LIST, ControlKind::ListBox),
            create_ctrl(w!("BUTTON"), w!("新規"), BS_PUSHBUTTON as u32,
                28, 420, 64, 32, IDC_SHORTCUT_NEW, ControlKind::Standard),
            create_ctrl(w!("BUTTON"), w!("削除"), BS_PUSHBUTTON as u32,
                100, 420, 64, 32, IDC_SHORTCUT_DELETE, ControlKind::Standard),
            create_ctrl(w!("BUTTON"), w!("上へ"), BS_PUSHBUTTON as u32,
                172, 420, 64, 32, IDC_SHORTCUT_UP, ControlKind::Standard),
            create_ctrl(w!("BUTTON"), w!("下へ"), BS_PUSHBUTTON as u32,
                244, 420, 64, 32, IDC_SHORTCUT_DOWN, ControlKind::Standard),
            create_ctrl(w!("STATIC"), w!("表示名"), 0, 330, 50, 210, 22, 0, ControlKind::Standard),
            create_ctrl(w!("EDIT"), w!(""), WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                330, 76, 454, 28, IDC_SHORTCUT_NAME, ControlKind::Standard),
            create_ctrl(w!("STATIC"), w!("種類"), 0, 330, 134, 210, 22, 0, ControlKind::Standard),
            shortcut_kind,
            create_ctrl(w!("STATIC"), w!("場所（絶対パスまたは URL）"), 0,
                330, 214, 420, 22, 0, ControlKind::Standard),
            create_ctrl(w!("EDIT"), w!(""), WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                330, 240, 368, 28, IDC_SHORTCUT_TARGET, ControlKind::Standard),
            create_ctrl(w!("BUTTON"), w!("参照…"), BS_PUSHBUTTON as u32,
                706, 240, 78, 28, IDC_SHORTCUT_BROWSE, ControlKind::Standard),
            create_ctrl(w!("BUTTON"), w!("リストに反映"), BS_PUSHBUTTON as u32,
                330, 290, 144, 32, IDC_SHORTCUT_UPDATE, ControlKind::Standard),
        ];
        for kind in ShortcutKind::ALL {
            let text: Vec<u16> = kind.label().encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(shortcut_kind, CB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
        }

        self.control_layouts = layouts;

        self.apply_dpi_and_layout(dpi);
        self.populate_data();
        self.select_shortcut(None);
        self.select_tab(tab as usize);
    }

    pub unsafe fn apply_dpi_and_layout(&mut self, new_dpi: u32) {
        use crate::platform::monitors::dip_to_px;
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::Graphics::Gdi::{DeleteObject, InvalidateRect, HGDIOBJ};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
        };

        let new_dpi = new_dpi.max(72);
        self.dpi = new_dpi;

        let new_font = create_settings_ui_font(new_dpi);
        let old_font = self.hfont;
        self.hfont = new_font;

        let list_item_h = dip_to_px(22, new_dpi);
        let combo_field_h = dip_to_px(22, new_dpi);
        let combo_item_h = dip_to_px(20, new_dpi);

        for item in &self.control_layouts {
            let _ = SendMessageW(
                item.hwnd,
                0x0030, /* WM_SETFONT */
                WPARAM(new_font.0 as usize),
                LPARAM(1),
            );

            let x = dip_to_px(item.x, new_dpi);
            let y = dip_to_px(item.y, new_dpi);
            let w = dip_to_px(item.w, new_dpi);
            let h = dip_to_px(item.h, new_dpi);

            match item.kind {
                ControlKind::ListBox => {
                    // LB_SETITEMHEIGHT = 0x01A0, LB_SETHORIZONTALEXTENT = 0x0194
                    let _ =
                        SendMessageW(item.hwnd, 0x01A0, WPARAM(0), LPARAM(list_item_h as isize));
                    let _ = SendMessageW(
                        item.hwnd,
                        0x0194,
                        WPARAM(dip_to_px(1200, new_dpi) as usize),
                        LPARAM(0),
                    );
                }
                ControlKind::ComboBox => {
                    // CB_SETITEMHEIGHT = 0x0153 (-1 = selection field, 0 = list items)
                    let _ = SendMessageW(
                        item.hwnd,
                        0x0153,
                        WPARAM(usize::MAX),
                        LPARAM(combo_field_h as isize),
                    );
                    let _ = SendMessageW(
                        item.hwnd,
                        0x0153,
                        WPARAM(0),
                        LPARAM(combo_item_h as isize),
                    );
                }
                ControlKind::TabControl | ControlKind::Standard | ControlKind::ListView => {}
            }

            let _ = SetWindowPos(
                item.hwnd,
                HWND::default(),
                x,
                y,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }

        if !old_font.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(old_font.0));
        }

        let _ = InvalidateRect(self.hwnd, None, true);
    }

    pub unsafe fn clamp_to_current_monitor_work_area(&mut self) {
        use crate::config::RectData;
        use crate::platform::monitors::find_best_monitor_for_rect;
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
        };

        if self.applying_dpi {
            return;
        }
        self.applying_dpi = true;

        // Up to 2 passes in case clamping a straddling window into a monitor's work_area
        // shifts MonitorFromWindow onto that monitor and changes GetDpiForWindow(hwnd).
        for _ in 0..2 {
            let mut rc = RECT::default();
            if GetWindowRect(self.hwnd, &mut rc).is_err() {
                break;
            }
            let rect_data = RectData {
                left: rc.left,
                top: rc.top,
                right: rc.right,
                bottom: rc.bottom,
            };
            let mon = find_best_monitor_for_rect(&rect_data, &self.vm.monitors).cloned();
            let raw_dpi = {
                let d = GetDpiForWindow(self.hwnd);
                if d > 0 {
                    d
                } else {
                    mon.as_ref().map(|m| m.dpi).unwrap_or(self.dpi)
                }
            };
            let eff_dpi = compute_effective_dpi(raw_dpi, mon.as_ref().map(|m| m.work_area));
            if eff_dpi != self.dpi {
                self.apply_dpi_and_layout(eff_dpi);
            }

            let (outer_w, outer_h) = compute_settings_outer_size(self.dpi);
            let (new_x, new_y) = if let Some(ref m) = mon {
                let max_x = (m.work_area.right - outer_w).max(m.work_area.left);
                let max_y = (m.work_area.bottom - outer_h).max(m.work_area.top);
                (
                    rc.left.clamp(m.work_area.left, max_x),
                    rc.top.clamp(m.work_area.top, max_y),
                )
            } else {
                (rc.left, rc.top)
            };

            let cur_w = rc.right - rc.left;
            let cur_h = rc.bottom - rc.top;
            if new_x != rc.left || new_y != rc.top || cur_w != outer_w || cur_h != outer_h {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND::default(),
                    new_x,
                    new_y,
                    outer_w,
                    outer_h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            } else {
                break;
            }
        }

        self.applying_dpi = false;
    }

    pub fn switch_tab(&mut self, active_idx: usize) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOW};
        for (idx, list) in self.tab_controls.iter().enumerate() {
            let cmd = if idx == active_idx { SW_SHOW } else { SW_HIDE };
            for &ctrl in list {
                unsafe {
                    let _ = ShowWindow(ctrl, cmd);
                }
            }
        }
    }

    fn select_tab(&mut self, index: usize) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::Controls::TCM_SETCURSEL;
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

        unsafe {
            let _ = SendMessageW(dlg_item(self.hwnd, IDC_TAB_CONTROL), TCM_SETCURSEL, WPARAM(index), LPARAM(0));
        }
        self.switch_tab(index);
    }

    pub fn populate_data(&mut self) {
        self.populate_scenes_tab();
        self.populate_app_matrix();
        self.populate_launcher_tab();
        self.populate_behavior_tab();
    }

    pub fn populate_scenes_tab(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{LB_ADDSTRING, LB_RESETCONTENT, LB_SETCURSEL, SendMessageW};

        unsafe {
            let h_list = dlg_item(self.hwnd, IDC_SCENE_LIST);
            let _ = SendMessageW(h_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
            for scene in &self.vm.config_draft.scenes {
                let pin_mark = if scene.pinned { "📌" } else { "  " };
                let app_count = self
                    .vm
                    .binding_rows
                    .iter()
                    .filter(|b| b.bound_scene_ids.iter().any(|id| id == &scene.id))
                    .count();
                let text = format!("{} #{}: {} ({}アプリ)", pin_mark, scene.order, scene.name, app_count);
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_list, LB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as isize));
            }
            if !self.vm.config_draft.scenes.is_empty() {
                let cur = self.selected_scene_idx.unwrap_or(0).min(self.vm.config_draft.scenes.len().saturating_sub(1));
                let _ = SendMessageW(h_list, LB_SETCURSEL, WPARAM(cur), LPARAM(0));
                self.selected_scene_idx = Some(cur);
                self.load_selected_scene_details(cur);
            }
        }
    }

    pub fn load_selected_scene_details(&mut self, idx: usize) {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, SetWindowTextW, BM_SETCHECK, LB_ADDSTRING, LB_RESETCONTENT,
        };

        self.updating_fields = true;
        if let Some(scene) = self.vm.config_draft.scenes.get(idx).cloned() {
            let (hk_mods, hk_vk) = self.vm.get_scene_hotkey(&scene.id).unwrap_or((0, 0));
            let (snap_count, entry_count) = self.vm.scene_layout_summary(&scene.id);
            unsafe {
                let set_txt = |id: usize, val: &str| {
                    let h = dlg_item(self.hwnd, id);
                    let w: Vec<u16> = val.encode_utf16().chain(std::iter::once(0)).collect();
                    let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
                };
                set_txt(IDC_SCENE_ID, &scene.id);
                set_txt(IDC_SCENE_NAME, &scene.name);
                set_txt(IDC_SCENE_ICON, &scene.icon);
                self.sync_icon_preset_selection(&scene.icon);

                let h_chk = dlg_item(self.hwnd, IDC_SCENE_PINNED);
                let st = if scene.pinned { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h_chk, BM_SETCHECK, WPARAM(st), LPARAM(0));

                // Layout snapshot status + Delete button state
                let layout_msg = if snap_count > 0 {
                    format!(
                        "保存済みレイアウト: {}モニタ ({} ウィンドウ)",
                        snap_count, entry_count
                    )
                } else {
                    "保存済みレイアウト: なし".to_string()
                };
                set_txt(IDC_SCENE_LAYOUT_STATUS, &layout_msg);
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    dlg_item(self.hwnd, IDC_SCENE_LAYOUT_DELETE),
                    snap_count > 0,
                );

                // Hotkey controls (MOD_ALT=1, MOD_CONTROL=2, MOD_SHIFT=4, MOD_WIN=8)
                let set_hk_chk = |id: usize, flag: u32| {
                    let s = if (hk_mods & flag) != 0 {
                        BST_CHECKED
                    } else {
                        BST_UNCHECKED
                    };
                    let _ = SendMessageW(dlg_item(self.hwnd, id), BM_SETCHECK, WPARAM(s), LPARAM(0));
                };
                set_hk_chk(IDC_SCENE_HK_ALT, 0x0001);
                set_hk_chk(IDC_SCENE_HK_CTRL, 0x0002);
                set_hk_chk(IDC_SCENE_HK_SHIFT, 0x0004);
                set_hk_chk(IDC_SCENE_HK_WIN, 0x0008);

                let key_idx = HOTKEY_KEY_CHOICES
                    .iter()
                    .position(|&(vk, _)| vk == hk_vk)
                    .unwrap_or(0);
                let _ = SendMessageW(
                    dlg_item(self.hwnd, IDC_SCENE_HK_KEY),
                    windows::Win32::UI::WindowsAndMessaging::CB_SETCURSEL,
                    WPARAM(key_idx),
                    LPARAM(0),
                );
                update_scene_hotkey_warning(self.hwnd);

                // Populate scene apps list
                let h_apps_list = dlg_item(self.hwnd, IDC_SCENE_APPS_LIST);
                let _ = SendMessageW(h_apps_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
                for b in &self.vm.binding_rows {
                    if b.bound_scene_ids.iter().any(|id| id == &scene.id) {
                        let run_mark = if b.is_running { "● 起動中" } else { "○ 未起動" };
                        let av_mark = if b.always_visible { " [常時表示]" } else { "" };
                        let txt = format!(
                            "[{}] {} ({}) [所在: {}]{}",
                            run_mark, b.window_title, b.exe_name, b.monitor_label, av_mark
                        );
                        let w_txt: Vec<u16> = txt.encode_utf16().chain(std::iter::once(0)).collect();
                        let _ = SendMessageW(h_apps_list, LB_ADDSTRING, WPARAM(0), LPARAM(w_txt.as_ptr() as isize));
                    }
                }
            }
        }
        self.updating_fields = false;
    }

    unsafe fn sync_icon_preset_selection(&self, icon: &str) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, CB_SETCURSEL};
        let preset_idx = crate::launcher::paint::BUILTIN_ICONS
            .iter()
            .position(|(id, _)| *id == icon)
            .unwrap_or(crate::launcher::paint::BUILTIN_ICONS.len());
        let _ = SendMessageW(
            dlg_item(self.hwnd, IDC_SCENE_ICON_PRESET),
            CB_SETCURSEL,
            WPARAM(preset_idx),
            LPARAM(0),
        );
    }

    pub fn save_current_scene_fields(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, SendMessageW, BM_GETCHECK};

        if self.updating_fields {
            return;
        }

        if let Some(idx) = self.selected_scene_idx {
            let mut updated_scene_id: Option<String> = None;
            if let Some(scene) = self.vm.config_draft.scenes.get_mut(idx) {
                unsafe {
                    let get_text = |ctrl_id: usize| -> String {
                        let h = dlg_item(self.hwnd, ctrl_id);
                        let mut buf = [0u16; 512];
                        let len = GetWindowTextW(h, &mut buf);
                        if len > 0 {
                            String::from_utf16_lossy(&buf[..len as usize])
                        } else {
                            String::new()
                        }
                    };
                    let name = get_text(IDC_SCENE_NAME).trim().to_string();
                    let icon = get_text(IDC_SCENE_ICON).trim().to_string();
                    let h_chk = dlg_item(self.hwnd, IDC_SCENE_PINNED);
                    let pinned = SendMessageW(h_chk, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize;

                    if !name.is_empty() {
                        scene.name = name;
                    }
                    if !icon.is_empty() {
                        scene.icon = icon;
                    }
                    scene.pinned = pinned;
                    updated_scene_id = Some(scene.id.clone());
                }
            }
            if let Some(scene_id) = updated_scene_id {
                let (mods, vk_opt) = unsafe { read_hotkey_fields_from_ui(self.hwnd) };
                self.vm.set_scene_hotkey(&scene_id, mods, vk_opt);
            }
        }
    }

    pub fn populate_app_matrix(&mut self) {
        use crate::platform::monitors::dip_to_px;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::Controls::{
            LVCFMT_CENTER, LVCFMT_LEFT, LVCF_FMT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
            LVIF_PARAM, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETECOLUMN,
            LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETITEMTEXTW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

        let dpi = self.dpi.max(72);
        let h_matrix = dlg_item(self.hwnd, IDC_APP_MATRIX_LIST);

        unsafe {
            let _ = SendMessageW(h_matrix, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
            // Remove all existing columns
            while SendMessageW(h_matrix, LVM_DELETECOLUMN, WPARAM(0), LPARAM(0)).0 != 0 {}

            // Helper to insert column
            let insert_col = |col_idx: i32, title: &str, width_dip: i32, fmt: windows::Win32::UI::Controls::LVCOLUMNW_FORMAT| {
                let w_text: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
                let col = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT,
                    fmt,
                    cx: dip_to_px(width_dip, dpi),
                    pszText: windows::core::PWSTR(w_text.as_ptr() as *mut u16),
                    iSubItem: col_idx,
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    LVM_INSERTCOLUMNW,
                    WPARAM(col_idx as usize),
                    LPARAM(&col as *const _ as isize),
                );
            };

            // Col 0: 状態
            insert_col(0, "状態", 92, LVCFMT_LEFT);
            // Col 1: アプリ / ウィンドウ
            insert_col(1, "アプリ / ウィンドウ", 224, LVCFMT_LEFT);

            // Col 2..N: 各シーン
            let num_scenes = self.vm.config_draft.scenes.len();
            for (s_idx, scene) in self.vm.config_draft.scenes.iter().enumerate() {
                let col_idx = 2 + s_idx as i32;
                insert_col(col_idx, &scene.name, 68, LVCFMT_CENTER);
            }

            // Col 2 + N: 常時表示
            let col_av = 2 + num_scenes as i32;
            insert_col(col_av, "常時表示", 72, LVCFMT_CENTER);

            // Col 2 + N + 1: モニター
            let col_mon = col_av + 1;
            insert_col(col_mon, "モニター", 76, LVCFMT_LEFT);

            // Insert rows
            for (row_idx, row) in self.vm.binding_rows.iter().enumerate() {
                let ambig_cnt = row.ambiguous_candidates.len();
                let warn_sym = if ambig_cnt > 1 {
                    format!(" ⚠️候補({})", ambig_cnt)
                } else if !row.warnings.is_empty() {
                    " ⚠️".to_string()
                } else {
                    String::new()
                };
                let run_sym = if row.is_running { "● 起動中" } else { "○ 未起動" };
                let status_str = format!("{}{}", run_sym, warn_sym);
                let w_status: Vec<u16> = status_str.encode_utf16().chain(std::iter::once(0)).collect();

                let mut item = LVITEMW {
                    mask: LVIF_TEXT | LVIF_PARAM,
                    iItem: row_idx as i32,
                    iSubItem: 0,
                    pszText: windows::core::PWSTR(w_status.as_ptr() as *mut u16),
                    lParam: LPARAM(row_idx as isize),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    LVM_INSERTITEMW,
                    WPARAM(0),
                    LPARAM(&mut item as *mut _ as isize),
                );

                // Col 1: アプリ
                let app_str = format!("{} ({})", row.window_title, row.exe_name);
                let w_app: Vec<u16> = app_str.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub_app = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row_idx as i32,
                    iSubItem: 1,
                    pszText: windows::core::PWSTR(w_app.as_ptr() as *mut u16),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    LVM_SETITEMTEXTW,
                    WPARAM(row_idx),
                    LPARAM(&mut sub_app as *mut _ as isize),
                );

                // Col 2..N: シーン
                for (s_idx, scene) in self.vm.config_draft.scenes.iter().enumerate() {
                    let is_bound = row.bound_scene_ids.iter().any(|id| id == &scene.id);
                    let sym = if is_bound { "☑" } else { "☐" };
                    let w_sym: Vec<u16> = sym.encode_utf16().chain(std::iter::once(0)).collect();
                    let mut sub_s = LVITEMW {
                        mask: LVIF_TEXT,
                        iItem: row_idx as i32,
                        iSubItem: 2 + s_idx as i32,
                        pszText: windows::core::PWSTR(w_sym.as_ptr() as *mut u16),
                        ..Default::default()
                    };
                    let _ = SendMessageW(
                        h_matrix,
                        LVM_SETITEMTEXTW,
                        WPARAM(row_idx),
                        LPARAM(&mut sub_s as *mut _ as isize),
                    );
                }

                // Col 常時表示
                let av_sym = if row.always_visible { "☑" } else { "☐" };
                let w_av: Vec<u16> = av_sym.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub_av = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row_idx as i32,
                    iSubItem: col_av,
                    pszText: windows::core::PWSTR(w_av.as_ptr() as *mut u16),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    LVM_SETITEMTEXTW,
                    WPARAM(row_idx),
                    LPARAM(&mut sub_av as *mut _ as isize),
                );

                // Col モニター
                let w_mon: Vec<u16> = row.monitor_label.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub_mon = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row_idx as i32,
                    iSubItem: col_mon,
                    pszText: windows::core::PWSTR(w_mon.as_ptr() as *mut u16),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    LVM_SETITEMTEXTW,
                    WPARAM(row_idx),
                    LPARAM(&mut sub_mon as *mut _ as isize),
                );
            }

            if !self.vm.binding_rows.is_empty() {
                let target_idx = self
                    .selected_app_idx
                    .unwrap_or(0)
                    .min(self.vm.binding_rows.len().saturating_sub(1));
                self.selected_app_idx = Some(target_idx);

                // Set selection in ListView (LVM_SETITEMSTATE)
                let mut state_item = LVITEMW {
                    state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
                        windows::Win32::UI::Controls::LVIS_SELECTED.0
                            | windows::Win32::UI::Controls::LVIS_FOCUSED.0,
                    ),
                    stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
                        windows::Win32::UI::Controls::LVIS_SELECTED.0
                            | windows::Win32::UI::Controls::LVIS_FOCUSED.0,
                    ),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    h_matrix,
                    windows::Win32::UI::Controls::LVM_SETITEMSTATE,
                    WPARAM(target_idx),
                    LPARAM(&mut state_item as *mut _ as isize),
                );
                self.load_selected_app_details(target_idx);
            }
        }
    }

    pub fn load_selected_app_details(&mut self, idx: usize) {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, SetWindowTextW, BM_SETCHECK};

        if let Some(row) = self.vm.binding_rows.get(idx) {
            let match_mon_checked = row.monitor_id.is_some();
            let kw_text = row.custom_title_keyword.clone();

            let status_desc = if row.is_running {
                format!(
                    "【起動中】 HWND: 0x{:X} / PID: {} / 所在: {}",
                    row.live_hwnd.unwrap_or(0),
                    row.live_pid.unwrap_or(0),
                    row.monitor_label
                )
            } else {
                format!("【未起動】 再起動時に自動照合 / 設定モニター: {}", row.monitor_label)
            };

            let bound_desc = if row.bound_scene_ids.is_empty() {
                "なし (どのシーンでも非表示)".to_string()
            } else {
                row.bound_scene_ids.join(", ")
            };

            let mut details = format!(
                "{}\r\nEXE: {} (Class: {})\r\n所属シーン: {} | 常時表示: {}\r\n",
                status_desc,
                row.exe_path,
                row.window_class,
                bound_desc,
                if row.always_visible { "有効" } else { "無効" }
            );

            if !row.warnings.is_empty() {
                details.push_str("【警告】");
                for w in &row.warnings {
                    match w {
                        crate::scenes::bindings::RegistrationWarning::NoTaskbarButton => {
                            details.push_str(" ⚠️タスクバーボタンなし ");
                        }
                        crate::scenes::bindings::RegistrationWarning::ElevatedProcess => {
                            details.push_str(" ⚠️管理者権限プロセス ");
                        }
                        crate::scenes::bindings::RegistrationWarning::OutsideTargetMonitor { window_monitor } => {
                            details.push_str(&format!(" ⚠️対象外モニター({}) ", window_monitor));
                        }
                        crate::scenes::bindings::RegistrationWarning::TitleMayContainSensitiveOrDynamicText => {
                            details.push_str(" ℹ️動的タイトル ");
                        }
                        crate::scenes::bindings::RegistrationWarning::AmbiguousSameMonitor { count } => {
                            details.push_str(&format!(
                                " ⚠️同一モニタに候補{}個（「候補を特定…」で指定可） ",
                                count
                            ));
                        }
                        crate::scenes::bindings::RegistrationWarning::AmbiguousCrossMonitor { count } => {
                            details.push_str(&format!(
                                " ⚠️複数モニタに候補{}個（「候補を特定…」で指定可） ",
                                count
                            ));
                        }
                    }
                }
                details.push_str("\r\n");
            }

            let has_ambig = !row.ambiguous_candidates.is_empty();
            unsafe {
                let h_det = dlg_item(self.hwnd, IDC_APP_DETAILS);
                let w: Vec<u16> = details.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(h_det, PCWSTR(w.as_ptr()));

                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    dlg_item(self.hwnd, IDC_APP_RESOLVE),
                    has_ambig,
                );

                let h_kw = dlg_item(self.hwnd, IDC_APP_TITLE_KEYWORD);
                let w_kw: Vec<u16> = kw_text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(h_kw, PCWSTR(w_kw.as_ptr()));

                let h_mon_chk = dlg_item(self.hwnd, IDC_APP_MATCH_MONITOR);
                let st_mon = if match_mon_checked { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h_mon_chk, BM_SETCHECK, WPARAM(st_mon), LPARAM(0));
            }
        }
    }

    pub fn save_current_app_rule(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, SendMessageW, BM_GETCHECK};

        if let Some(idx) = self.selected_app_idx {
            if let Some(row) = self.vm.binding_rows.get(idx) {
                let binding_id = row.binding_id.clone();
                let mon_id = row.monitor_id.clone();
                unsafe {
                    let h_kw = dlg_item(self.hwnd, IDC_APP_TITLE_KEYWORD);
                    let mut buf = [0u16; 256];
                    let len = GetWindowTextW(h_kw, &mut buf);
                    let custom_kw = if len > 0 {
                        Some(String::from_utf16_lossy(&buf[..len as usize]))
                    } else {
                        None
                    };

                    let h_mon_chk = dlg_item(self.hwnd, IDC_APP_MATCH_MONITOR);
                    let pin_to_monitor =
                        SendMessageW(h_mon_chk, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                            == BST_CHECKED as isize;

                    self.vm.update_binding_title_keyword(&binding_id, custom_kw);
                    self.vm.update_binding_monitor_pin(&binding_id, pin_to_monitor, mon_id);
                }
            }
        }
    }

    pub fn populate_launcher_tab(&mut self) {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, SetWindowTextW, BM_SETCHECK, CB_ADDSTRING, CB_RESETCONTENT, CB_SETCURSEL,
        };

        unsafe {
            let h_orient = dlg_item(self.hwnd, IDC_LCH_ORIENT_COMBO);
            let _ = SendMessageW(h_orient, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
            let s_h: Vec<u16> = "横型 (Horizontal)".encode_utf16().chain(std::iter::once(0)).collect();
            let s_v: Vec<u16> = "縦型 (Vertical)".encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(h_orient, CB_ADDSTRING, WPARAM(0), LPARAM(s_h.as_ptr() as isize));
            let _ = SendMessageW(h_orient, CB_ADDSTRING, WPARAM(0), LPARAM(s_v.as_ptr() as isize));
            let sel = match self.vm.config_draft.launcher.orientation {
                Orientation::Horizontal => 0,
                Orientation::Vertical => 1,
            };
            let _ = SendMessageW(h_orient, CB_SETCURSEL, WPARAM(sel), LPARAM(0));

            let set_edit = |id: usize, val: i32| {
                let h = dlg_item(self.hwnd, id);
                let w_val: Vec<u16> = val.to_string().encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(h, PCWSTR(w_val.as_ptr()));
            };
            set_edit(IDC_LCH_BTN_SIZE, self.vm.config_draft.launcher.button_size_dip);
            set_edit(IDC_LCH_GRIP_SIZE, self.vm.config_draft.launcher.grip_size_dip);
            set_edit(IDC_LCH_SLOTS, self.vm.config_draft.launcher.visible_slots as i32);

            let set_chk = |id: usize, b: bool| {
                let h = dlg_item(self.hwnd, id);
                let state = if b { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h, BM_SETCHECK, WPARAM(state), LPARAM(0));
            };
            set_chk(IDC_LCH_TOPMOST, self.vm.config_draft.launcher.always_on_top);
            set_chk(IDC_LCH_SNAP, self.vm.config_draft.launcher.snap_to_edges);
            set_chk(IDC_LCH_FOCUS, self.vm.config_draft.launcher.focus_on_switch);
            set_chk(IDC_LCH_WHEEL_WRAP, self.vm.config_draft.launcher.wheel_wrap);
            set_chk(IDC_LCH_SHOW_BAR, self.vm.config_draft.launcher.show_bar);
            set_chk(IDC_LCH_CTRL_DOUBLE_TAP, self.vm.config_draft.launcher.ctrl_double_tap);
        }
    }

    unsafe fn populate_shortcut_list(&self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, LB_ADDSTRING, LB_RESETCONTENT, LB_SETCURSEL,
        };

        let list = dlg_item(self.hwnd, IDC_SHORTCUT_LIST);
        let _ = SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        let shortcuts = &self.vm.config_draft.shortcuts;
        for shortcut in shortcuts {
            let text: Vec<u16> = format!("{}  ·  {}", shortcut.name, shortcut.kind.label())
                .encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
        }
        let index = self.selected_shortcut_idx;
        let _ = SendMessageW(list, LB_SETCURSEL, WPARAM(index.unwrap_or(usize::MAX)), LPARAM(0));
        let _ = EnableWindow(dlg_item(self.hwnd, IDC_SHORTCUT_DELETE), index.is_some());
        let _ = EnableWindow(dlg_item(self.hwnd, IDC_SHORTCUT_UP), index.is_some_and(|i| i > 0));
        let _ = EnableWindow(dlg_item(self.hwnd, IDC_SHORTCUT_DOWN), index.is_some_and(|i| i + 1 < shortcuts.len()));
    }

    unsafe fn select_shortcut(&mut self, index: Option<usize>) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, CB_SETCURSEL};

        self.selected_shortcut_idx = index;
        let (name, kind, target) = index.and_then(|i| self.vm.config_draft.shortcuts.get(i))
            .map(|shortcut| (shortcut.name.as_str(), shortcut.kind, shortcut.target.as_str()))
            .unwrap_or(("", ShortcutKind::Folder, ""));
        set_edit_text(self.hwnd, IDC_SHORTCUT_NAME, name);
        set_edit_text(self.hwnd, IDC_SHORTCUT_TARGET, target);
        let _ = SendMessageW(dlg_item(self.hwnd, IDC_SHORTCUT_KIND), CB_SETCURSEL, WPARAM(kind as usize), LPARAM(0));
        self.update_shortcut_browse_button();
        self.populate_shortcut_list();
    }

    unsafe fn shortcut_kind(&self) -> ShortcutKind {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, CB_GETCURSEL};

        let combo = dlg_item(self.hwnd, IDC_SHORTCUT_KIND);
        let index = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
        ShortcutKind::ALL.get(index).copied().unwrap_or(ShortcutKind::Folder)
    }

    unsafe fn update_shortcut_browse_button(&self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
        let _ = EnableWindow(dlg_item(self.hwnd, IDC_SHORTCUT_BROWSE), self.shortcut_kind() != ShortcutKind::Website);
    }

    unsafe fn save_shortcut_fields(&mut self, required: bool) -> Result<(), &'static str> {
        let name = edit_text(self.hwnd, IDC_SHORTCUT_NAME).trim().to_owned();
        let target = edit_text(self.hwnd, IDC_SHORTCUT_TARGET);
        let target = target.trim();
        if !required && self.selected_shortcut_idx.is_none() && name.is_empty() && target.is_empty() {
            return Ok(());
        }

        let kind = self.shortcut_kind();
        // Explorer's Copy as path command surrounds the path with quotes.
        let target = if kind == ShortcutKind::Website { target } else {
            target.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(target)
        };
        let shortcut = Shortcut { name, kind, target: target.to_owned() };
        shortcut.validate()?;
        let shortcuts = &mut self.vm.config_draft.shortcuts;
        if let Some(index) = self.selected_shortcut_idx {
            shortcuts[index] = shortcut;
        } else {
            self.selected_shortcut_idx = Some(shortcuts.len());
            shortcuts.push(shortcut);
        }
        Ok(())
    }

    pub fn populate_behavior_tab(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, BM_SETCHECK, CB_ADDSTRING, CB_RESETCONTENT, CB_SETCURSEL,
        };

        unsafe {
            let h_mon_combo = dlg_item(self.hwnd, IDC_BEH_MONITOR_COMBO);
            let _ = SendMessageW(h_mon_combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));

            let s_all: Vec<u16> = "すべてのモニター (左右/全画面の登録ウィンドウを切替)"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let _ = SendMessageW(h_mon_combo, CB_ADDSTRING, WPARAM(0), LPARAM(s_all.as_ptr() as isize));

            let mut cur_mon_sel = 0; // Index 0 = AllMonitors
            for (idx, mon) in self.vm.monitors.iter().enumerate() {
                let prim = if mon.is_primary { " (プライマリ)" } else { "" };
                let pos_label = self.vm.format_monitor_short_label(&mon.monitor_id);
                let txt = format!("単一固定 [{}]: {} ({}){}", pos_label, mon.friendly_name, mon.monitor_id, prim);
                let w_txt: Vec<u16> = txt.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_mon_combo, CB_ADDSTRING, WPARAM(0), LPARAM(w_txt.as_ptr() as isize));
                if self.vm.config_draft.target.monitor_policy == MonitorPolicy::Fixed
                    && self.vm.config_draft.target.monitor_id.as_deref() == Some(&mon.monitor_id)
                {
                    cur_mon_sel = idx + 1;
                }
            }
            let s_follow: Vec<u16> = "ランチャー所在モニターに自動追従".encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(h_mon_combo, CB_ADDSTRING, WPARAM(0), LPARAM(s_follow.as_ptr() as isize));
            if self.vm.config_draft.target.monitor_policy == MonitorPolicy::LauncherMonitor {
                cur_mon_sel = self.vm.monitors.len() + 1;
            }
            let _ = SendMessageW(h_mon_combo, CB_SETCURSEL, WPARAM(cur_mon_sel), LPARAM(0));

            let set_chk = |id: usize, b: bool| {
                let h = dlg_item(self.hwnd, id);
                let state = if b { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h, BM_SETCHECK, WPARAM(state), LPARAM(0));
            };
            set_chk(IDC_BEH_RESTORE_ALL_ON_SWITCH, self.vm.config_draft.launcher.restore_all_on_switch);
            set_chk(IDC_BEH_AUTOSTART, self.vm.autostart_enabled);
            set_chk(IDC_BEH_VERBOSE_TITLE, crate::diagnostics::is_verbose_title_logging());
        }
    }

    pub fn save_changes_to_config(&mut self) -> Result<(), String> {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowTextW, SendMessageW, BM_GETCHECK, CB_GETCURSEL,
        };

        self.save_current_scene_fields();
        self.save_current_app_rule();

        unsafe {
            self.save_shortcut_fields(false)?;
            self.populate_shortcut_list();
            // Read Launcher controls
            let h_orient = dlg_item(self.hwnd, IDC_LCH_ORIENT_COMBO);
            let orient_sel = SendMessageW(h_orient, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
            self.vm.config_draft.launcher.orientation = if orient_sel == 1 {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            };
            let h_popup_position = dlg_item(self.hwnd, IDC_LCH_POPUP_POSITION);
            let popup_sel = SendMessageW(h_popup_position, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
            if let Some(&(position, _)) = CURSOR_POPUP_POSITION_CHOICES.get(popup_sel as usize) {
                self.vm.config_draft.launcher.cursor_popup_position = position;
            }

            let get_int = |id: usize, def: i32| -> i32 {
                let h = dlg_item(self.hwnd, id);
                let mut buf = [0u16; 32];
                let len = GetWindowTextW(h, &mut buf);
                if len > 0 {
                    String::from_utf16_lossy(&buf[..len as usize])
                        .trim()
                        .parse::<i32>()
                        .unwrap_or(def)
                } else {
                    def
                }
            };
            self.vm.config_draft.launcher.button_size_dip = get_int(IDC_LCH_BTN_SIZE, 32);
            self.vm.config_draft.launcher.grip_size_dip = get_int(IDC_LCH_GRIP_SIZE, 18);
            self.vm.config_draft.launcher.visible_slots = get_int(IDC_LCH_SLOTS, 3).max(1) as usize;
            self.vm.config_draft.validate_and_clamp();
            set_edit_text(
                self.hwnd,
                IDC_LCH_BTN_SIZE,
                &self.vm.config_draft.launcher.button_size_dip.to_string(),
            );
            set_edit_text(
                self.hwnd,
                IDC_LCH_GRIP_SIZE,
                &self.vm.config_draft.launcher.grip_size_dip.to_string(),
            );
            set_edit_text(
                self.hwnd,
                IDC_LCH_SLOTS,
                &self.vm.config_draft.launcher.visible_slots.to_string(),
            );

            let get_chk = |id: usize| -> bool {
                let h = dlg_item(self.hwnd, id);
                SendMessageW(h, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize
            };
            self.vm.config_draft.launcher.always_on_top = get_chk(IDC_LCH_TOPMOST);
            self.vm.config_draft.launcher.snap_to_edges = get_chk(IDC_LCH_SNAP);
            self.vm.config_draft.launcher.focus_on_switch = get_chk(IDC_LCH_FOCUS);
            self.vm.config_draft.launcher.wheel_wrap = get_chk(IDC_LCH_WHEEL_WRAP);
            self.vm.config_draft.launcher.show_bar = get_chk(IDC_LCH_SHOW_BAR);
            self.vm.config_draft.launcher.ctrl_double_tap = get_chk(IDC_LCH_CTRL_DOUBLE_TAP);
            self.vm.config_draft.launcher.restore_all_on_switch = get_chk(IDC_BEH_RESTORE_ALL_ON_SWITCH);

            // Read Behavior controls
            let h_mon = dlg_item(self.hwnd, IDC_BEH_MONITOR_COMBO);
            let mon_sel = SendMessageW(h_mon, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
            if mon_sel == 0 {
                self.vm.config_draft.target.monitor_policy = MonitorPolicy::AllMonitors;
            } else if mon_sel <= self.vm.monitors.len() {
                self.vm.config_draft.target.monitor_policy = MonitorPolicy::Fixed;
                self.vm.config_draft.target.monitor_id =
                    Some(self.vm.monitors[mon_sel - 1].monitor_id.clone());
            } else {
                self.vm.config_draft.target.monitor_policy = MonitorPolicy::LauncherMonitor;
            }

            let autostart = get_chk(IDC_BEH_AUTOSTART);
            let _ = set_autostart_enabled(autostart);

            let verbose_titles = get_chk(IDC_BEH_VERBOSE_TITLE);
            crate::diagnostics::set_verbose_title_logging(verbose_titles);
        }

        Ok(())
    }
}

unsafe fn edit_text(hwnd: windows::Win32::Foundation::HWND, id: usize) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    let control = dlg_item(hwnd, id);
    let mut wide = vec![0; GetWindowTextLengthW(control).max(0) as usize + 1];
    let length = GetWindowTextW(control, &mut wide).max(0) as usize;
    String::from_utf16_lossy(&wide[..length])
}

unsafe fn set_edit_text(hwnd: windows::Win32::Foundation::HWND, id: usize, text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::SetWindowTextW;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SetWindowTextW(dlg_item(hwnd, id), windows::core::PCWSTR(wide.as_ptr()));
}

unsafe fn read_hotkey_fields_from_ui(hwnd: windows::Win32::Foundation::HWND) -> (u32, Option<u32>) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, BM_GETCHECK, CB_GETCURSEL};

    let chk = |id: usize| -> bool {
        SendMessageW(dlg_item(hwnd, id), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize
    };
    let mut mods = 0u32;
    if chk(IDC_SCENE_HK_ALT) {
        mods |= 0x0001;
    }
    if chk(IDC_SCENE_HK_CTRL) {
        mods |= 0x0002;
    }
    if chk(IDC_SCENE_HK_SHIFT) {
        mods |= 0x0004;
    }
    if chk(IDC_SCENE_HK_WIN) {
        mods |= 0x0008;
    }
    let idx = SendMessageW(dlg_item(hwnd, IDC_SCENE_HK_KEY), CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let vk = HOTKEY_KEY_CHOICES
        .get(idx.max(0) as usize)
        .map(|&(v, _)| v)
        .filter(|&v| v != 0);
    (mods, vk)
}

unsafe fn update_scene_hotkey_warning(hwnd: windows::Win32::Foundation::HWND) {
    let (mods, vk) = read_hotkey_fields_from_ui(hwnd);
    let warn = if vk.is_some() && mods == 0 {
        "⚠️ 修飾キーなしは競合リスクあり"
    } else {
        ""
    };
    set_edit_text(hwnd, IDC_SCENE_HK_WARN, warn);
}

unsafe fn open_file_dialog(
    hwnd: windows::Win32::Foundation::HWND,
    pick_folders: bool,
    filters: &[(&str, &str)],
) -> windows::core::Result<Option<String>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
    let mode = if pick_folders {
        FOS_PICKFOLDERS
    } else {
        FOS_FILEMUSTEXIST
    };
    dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | mode)?;

    let owned_pairs: Vec<(HSTRING, HSTRING)> = filters
        .iter()
        .map(|(n, p)| (HSTRING::from(*n), HSTRING::from(*p)))
        .collect();
    if !owned_pairs.is_empty() && !pick_folders {
        let specs: Vec<COMDLG_FILTERSPEC> = owned_pairs
            .iter()
            .map(|(n, p)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(n.as_ptr()),
                pszSpec: PCWSTR(p.as_ptr()),
            })
            .collect();
        let _ = dialog.SetFileTypes(&specs);
    }

    if let Err(error) = dialog.Show(hwnd) {
        return if error.code() == ERROR_CANCELLED.to_hresult() {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
    let text = path.to_string();
    CoTaskMemFree(Some(path.0.cast()));
    Ok(Some(text?))
}

unsafe fn save_file_dialog(
    hwnd: windows::Win32::Foundation::HWND,
    default_name: &str,
    default_ext: &str,
    filters: &[(&str, &str)],
) -> windows::core::Result<Option<String>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        SIGDN_FILESYSPATH,
    };

    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
    dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)?;
    let _ = dialog.SetFileName(&HSTRING::from(default_name));
    let _ = dialog.SetDefaultExtension(&HSTRING::from(default_ext));

    let owned_pairs: Vec<(HSTRING, HSTRING)> = filters
        .iter()
        .map(|(n, p)| (HSTRING::from(*n), HSTRING::from(*p)))
        .collect();
    if !owned_pairs.is_empty() {
        let specs: Vec<COMDLG_FILTERSPEC> = owned_pairs
            .iter()
            .map(|(n, p)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(n.as_ptr()),
                pszSpec: PCWSTR(p.as_ptr()),
            })
            .collect();
        let _ = dialog.SetFileTypes(&specs);
    }

    if let Err(error) = dialog.Show(hwnd) {
        return if error.code() == ERROR_CANCELLED.to_hresult() {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
    let text = path.to_string();
    CoTaskMemFree(Some(path.0.cast()));
    Ok(Some(text?))
}

unsafe fn choose_shortcut_path(
    hwnd: windows::Win32::Foundation::HWND,
    kind: ShortcutKind,
) -> windows::core::Result<Option<String>> {
    open_file_dialog(hwnd, kind == ShortcutKind::Folder, &[])
}

unsafe fn show_settings_error(hwnd: windows::Win32::Foundation::HWND, text: &str) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = MessageBoxW(hwnd, PCWSTR(wide.as_ptr()), w!("Kiri 設定"), MB_OK | MB_ICONWARNING);
}

#[cfg(windows)]
unsafe extern "system" fn settings_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::core::w;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Controls::NMHDR;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetWindowLongPtrW, MessageBoxW, PostMessageW,
        SendMessageW, SetWindowLongPtrW, BM_GETCHECK, CBN_SELCHANGE, GWLP_USERDATA, LBN_SELCHANGE, LB_GETCURSEL,
        MB_ICONINFORMATION, MB_OK, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED,
        WM_EXITSIZEMOVE, WM_NCDESTROY, WM_NOTIFY,
    };

    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsWindow;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let win = &mut *ptr;

        match msg {
            WM_APP_SHOW_SETTINGS_TAB if wparam.0 < win.tab_controls.len() => {
                win.save_current_scene_fields();
                win.save_current_app_rule();
                win.select_tab(wparam.0);
                win.populate_data();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let new_dpi = ((wparam.0 & 0xFFFF) as u32).max(96);
                if !win.applying_dpi {
                    win.applying_dpi = true;
                    let (outer_w, outer_h) = compute_settings_outer_size(new_dpi);
                    if lparam.0 != 0 {
                        let suggested = &*(lparam.0 as *const windows::Win32::Foundation::RECT);
                        let suggested_w = suggested.right - suggested.left;
                        let left = suggested.left + (suggested_w - outer_w) / 2;
                        let top = suggested.top;
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                            hwnd,
                            windows::Win32::Foundation::HWND::default(),
                            left,
                            top,
                            outer_w,
                            outer_h,
                            windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                                | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                        );
                    }
                    win.apply_dpi_and_layout(new_dpi);
                    win.applying_dpi = false;
                }
                LRESULT(0)
            }

            WM_EXITSIZEMOVE => {
                win.clamp_to_current_monitor_work_area();
                LRESULT(0)
            }

            WM_NOTIFY => {
                let nmhdr = &*(lparam.0 as *const NMHDR);
                if nmhdr.idFrom == IDC_TAB_CONTROL
                    && nmhdr.code == windows::Win32::UI::Controls::TCN_SELCHANGE
                {
                    win.save_current_scene_fields();
                    win.save_current_app_rule();
                    let cur_tab = SendMessageW(
                        nmhdr.hwndFrom,
                        windows::Win32::UI::Controls::TCM_GETCURSEL,
                        WPARAM(0),
                        LPARAM(0),
                    )
                    .0 as usize;
                    win.switch_tab(cur_tab);
                    win.populate_data();
                    return LRESULT(0);
                }

                if nmhdr.idFrom == IDC_APP_MATRIX_LIST
                    && nmhdr.code == NM_CLICK
                {
                    let item_act = &*(lparam.0 as *const NMITEMACTIVATE);
                    let row_idx = item_act.iItem;
                    let col_idx = item_act.iSubItem;
                    if row_idx >= 0 && (row_idx as usize) < win.vm.binding_rows.len() {
                        let row_u = row_idx as usize;
                        let binding_id = win.vm.binding_rows[row_u].binding_id.clone();
                        let num_scenes = win.vm.config_draft.scenes.len();

                        if col_idx >= 2 && (col_idx as usize) < 2 + num_scenes {
                            let scene_idx = (col_idx - 2) as usize;
                            if let Some(scene) = win.vm.config_draft.scenes.get(scene_idx) {
                                let scene_id = scene.id.clone();
                                win.vm.toggle_scene_binding(&binding_id, &scene_id);
                                win.selected_app_idx = Some(row_u);
                                win.populate_app_matrix();
                                win.populate_scenes_tab();
                            }
                        } else if col_idx as usize == 2 + num_scenes {
                            win.vm.toggle_always_visible(&binding_id);
                            win.selected_app_idx = Some(row_u);
                            win.populate_app_matrix();
                            win.populate_scenes_tab();
                        } else {
                            win.selected_app_idx = Some(row_u);
                            win.load_selected_app_details(row_u);
                        }
                    }
                    return LRESULT(0);
                }

                if nmhdr.idFrom == IDC_APP_MATRIX_LIST && nmhdr.code == LVN_ITEMCHANGED {
                    let nmlv = &*(lparam.0 as *const NMLISTVIEW);
                    if (nmlv.uNewState & windows::Win32::UI::Controls::LVIS_SELECTED.0) != 0 && nmlv.iItem >= 0 {
                        let sel = nmlv.iItem as usize;
                        if sel < win.vm.binding_rows.len() {
                            win.selected_app_idx = Some(sel);
                            win.load_selected_app_details(sel);
                        }
                    }
                }

                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as usize;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u32;

                match id {
                    IDC_BTN_OK | IDC_BTN_APPLY => {
                        if let Err(error) = win.save_changes_to_config() {
                            win.select_tab(SettingsTab::Shortcuts as usize);
                            show_settings_error(hwnd, &error);
                            return LRESULT(0);
                        }
                        if !win.vm.config_draft.launcher.show_bar
                            && !win.vm.config_draft.launcher.ctrl_double_tap
                        {
                            let ans = MessageBoxW(
                                hwnd,
                                w!("「固定バーを表示する」と「Ctrl の二度押し」の両方が無効になっています。\nトレイアイコンから操作できますが、このまま適用しますか？"),
                                w!("Kiri 設定 - 確認"),
                                windows::Win32::UI::WindowsAndMessaging::MB_YESNO
                                    | windows::Win32::UI::WindowsAndMessaging::MB_ICONWARNING,
                            );
                            if ans != windows::Win32::UI::WindowsAndMessaging::IDYES {
                                return LRESULT(0);
                            }
                        }
                        let config = Box::into_raw(Box::new(win.vm.config_draft.clone()));
                        if PostMessageW(win.parent_hwnd, WM_APP_SETTINGS_SAVED, WPARAM(config as usize), LPARAM(0)).is_err() {
                            drop(Box::from_raw(config));
                            show_settings_error(hwnd, "設定を適用できませんでした。");
                        } else if id == IDC_BTN_OK {
                            let _ = DestroyWindow(hwnd);
                        }
                        LRESULT(0)
                    }
                    IDC_BTN_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }

                    // Tab 0 Scene selection & live edit sync
                    IDC_SCENE_LIST if code == LBN_SELCHANGE => {
                        win.save_current_scene_fields();
                        let h = dlg_item(hwnd, IDC_SCENE_LIST);
                        let sel = SendMessageW(h, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                        win.selected_scene_idx = Some(sel);
                        win.load_selected_scene_details(sel);
                        LRESULT(0)
                    }
                    IDC_SCENE_NAME if code == EN_CHANGE && !win.updating_fields => {
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_ICON if code == EN_CHANGE && !win.updating_fields => {
                        let icon_val = edit_text(hwnd, IDC_SCENE_ICON).trim().to_string();
                        win.sync_icon_preset_selection(&icon_val);
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_ICON_PRESET if code == CBN_SELCHANGE && !win.updating_fields => {
                        let sel = SendMessageW(
                            dlg_item(hwnd, IDC_SCENE_ICON_PRESET),
                            windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
                            WPARAM(0),
                            LPARAM(0),
                        )
                        .0;
                        if let Some(&(preset_id, _)) =
                            crate::launcher::paint::BUILTIN_ICONS.get(sel.max(0) as usize)
                        {
                            win.updating_fields = true;
                            set_edit_text(hwnd, IDC_SCENE_ICON, preset_id);
                            win.updating_fields = false;
                            win.save_current_scene_fields();
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_ICON_BROWSE => {
                        match open_file_dialog(
                            hwnd,
                            false,
                            &[("画像 / アイコン (*.ico;*.png;*.bmp)", "*.ico;*.png;*.bmp")],
                        ) {
                            Ok(Some(path)) => {
                                win.updating_fields = true;
                                set_edit_text(hwnd, IDC_SCENE_ICON, &path);
                                win.sync_icon_preset_selection(&path);
                                win.updating_fields = false;
                                win.save_current_scene_fields();
                            }
                            Ok(None) => {}
                            Err(e) => show_settings_error(hwnd, &e.to_string()),
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_PINNED if !win.updating_fields => {
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_LAYOUT_DELETE => {
                        if let Some(idx) = win.selected_scene_idx {
                            if let Some(scene_id) =
                                win.vm.config_draft.scenes.get(idx).map(|s| s.id.clone())
                            {
                                let removed = win.vm.delete_scene_layouts(&scene_id);
                                if removed > 0 {
                                    win.load_selected_scene_details(idx);
                                }
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_HK_CTRL
                    | IDC_SCENE_HK_ALT
                    | IDC_SCENE_HK_SHIFT
                    | IDC_SCENE_HK_WIN
                        if !win.updating_fields =>
                    {
                        update_scene_hotkey_warning(hwnd);
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_HK_KEY if code == CBN_SELCHANGE && !win.updating_fields => {
                        update_scene_hotkey_warning(hwnd);
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_HK_CLEAR => {
                        win.updating_fields = true;
                        for cid in [
                            IDC_SCENE_HK_CTRL,
                            IDC_SCENE_HK_ALT,
                            IDC_SCENE_HK_SHIFT,
                            IDC_SCENE_HK_WIN,
                        ] {
                            let _ = SendMessageW(
                                dlg_item(hwnd, cid),
                                windows::Win32::UI::WindowsAndMessaging::BM_SETCHECK,
                                WPARAM(BST_UNCHECKED),
                                LPARAM(0),
                            );
                        }
                        let _ = SendMessageW(
                            dlg_item(hwnd, IDC_SCENE_HK_KEY),
                            windows::Win32::UI::WindowsAndMessaging::CB_SETCURSEL,
                            WPARAM(0),
                            LPARAM(0),
                        );
                        set_edit_text(hwnd, IDC_SCENE_HK_WARN, "");
                        win.updating_fields = false;
                        win.save_current_scene_fields();
                        LRESULT(0)
                    }
                    IDC_SCENE_ADD => {
                        win.save_current_scene_fields();
                        let next_num = win.vm.config_draft.scenes.len() + 1;
                        let mut new_id = format!("scene-{}", next_num);
                        let mut suffix = next_num;
                        while win.vm.config_draft.scenes.iter().any(|s| s.id == new_id) {
                            suffix += 1;
                            new_id = format!("scene-{}", suffix);
                        }
                        let new_order = win.vm.config_draft.scenes.len() as i32 + 1;
                        win.vm.config_draft.scenes.push(Scene {
                            id: new_id.clone(),
                            name: format!("新規シーン {}", new_order),
                            icon: "chart".to_string(),
                            order: new_order,
                            pinned: true,
                        });
                        win.selected_scene_idx = Some(win.vm.config_draft.scenes.len() - 1);
                        win.populate_data();
                        LRESULT(0)
                    }
                    IDC_SCENE_DEL => {
                        if let Some(idx) = win.selected_scene_idx {
                            if win.vm.config_draft.scenes.len() > 1 && idx < win.vm.config_draft.scenes.len() {
                                let del_id = win.vm.config_draft.scenes[idx].id.clone();
                                win.vm.config_draft.scenes.remove(idx);
                                for (i, s) in win.vm.config_draft.scenes.iter_mut().enumerate() {
                                    s.order = i as i32 + 1;
                                }
                                for b in &mut win.vm.config_draft.bindings {
                                    b.scene_ids.retain(|s| s != &del_id);
                                }
                                win.vm.config_draft.hotkeys.retain(|hk| hk.scene_id != del_id);
                                win.vm.config_draft.layouts.retain(|l| l.scene_id != del_id);
                                win.vm.sync_binding_annotations();
                                win.vm.sync_binding_rows();
                                win.selected_scene_idx = Some(idx.saturating_sub(1));
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_UP => {
                        if let Some(idx) = win.selected_scene_idx {
                            if idx > 0 && idx < win.vm.config_draft.scenes.len() {
                                win.save_current_scene_fields();
                                win.vm.config_draft.scenes.swap(idx, idx - 1);
                                for (i, s) in win.vm.config_draft.scenes.iter_mut().enumerate() {
                                    s.order = i as i32 + 1;
                                }
                                win.selected_scene_idx = Some(idx - 1);
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_DOWN => {
                        if let Some(idx) = win.selected_scene_idx {
                            if idx + 1 < win.vm.config_draft.scenes.len() {
                                win.save_current_scene_fields();
                                win.vm.config_draft.scenes.swap(idx, idx + 1);
                                for (i, s) in win.vm.config_draft.scenes.iter_mut().enumerate() {
                                    s.order = i as i32 + 1;
                                }
                                win.selected_scene_idx = Some(idx + 1);
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_UPDATE => {
                        win.save_current_scene_fields();
                        win.populate_data();
                        LRESULT(0)
                    }
                    IDC_SCENE_APP_UNBIND => {
                        if let Some(s_idx) = win.selected_scene_idx {
                            if let Some(scene) = win.vm.config_draft.scenes.get(s_idx) {
                                let scene_id = scene.id.clone();
                                let h_apps = dlg_item(hwnd, IDC_SCENE_APPS_LIST);
                                let sel_app = SendMessageW(h_apps, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as isize;
                                if sel_app >= 0 {
                                    let matching_bids: Vec<String> = win.vm.binding_rows
                                        .iter()
                                        .filter(|b| b.bound_scene_ids.iter().any(|id| id == &scene_id))
                                        .map(|b| b.binding_id.clone())
                                        .collect();
                                    if let Some(target_bid) = matching_bids.get(sel_app as usize) {
                                        win.vm.unbind_window_from_scene(target_bid, &scene_id);
                                        win.populate_data();
                                    }
                                }
                            }
                        }
                        LRESULT(0)
                    }

                    // Tab 1 Apps Matrix
                    IDC_APP_ADD_WINDOW => {
                        show_add_window_dialog(hwnd, win);
                        LRESULT(0)
                    }
                    IDC_APP_DELETE => {
                        if let Some(idx) = win.selected_app_idx {
                            if let Some(row) = win.vm.binding_rows.get(idx) {
                                let bid = row.binding_id.clone();
                                win.vm.remove_binding(&bid);
                                win.selected_app_idx = None;
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_APP_REFRESH => {
                        let sys = crate::platform::windows::Win32WindowSystem::new();
                        win.vm.refresh_candidates(&sys);
                        win.populate_data();
                        LRESULT(0)
                    }
                    IDC_APP_APPLY_RULE => {
                        win.save_current_app_rule();
                        win.populate_app_matrix();
                        LRESULT(0)
                    }
                    IDC_APP_RESOLVE => {
                        show_pick_candidate_dialog(hwnd, win);
                        LRESULT(0)
                    }

                    // Tab 2 Launcher
                    IDC_LCH_RESET_POS => {
                        win.vm.config_draft.launcher.position = None;
                        let _ = MessageBoxW(hwnd, w!("ランチャー位置を初期化しました。[適用] または [OK] で反映されます。"), w!("Kiri"), MB_OK | MB_ICONINFORMATION);
                        LRESULT(0)
                    }
                    IDC_LCH_SHOW_BAR | IDC_LCH_CTRL_DOUBLE_TAP | IDC_LCH_WHEEL_WRAP => {
                        let checked = SendMessageW(
                            dlg_item(hwnd, id),
                            BM_GETCHECK,
                            WPARAM(0),
                            LPARAM(0),
                        ).0 == BST_CHECKED as isize;
                        let launcher = &mut win.vm.config_draft.launcher;
                        match id {
                            IDC_LCH_SHOW_BAR => launcher.show_bar = checked,
                            IDC_LCH_CTRL_DOUBLE_TAP => launcher.ctrl_double_tap = checked,
                            IDC_LCH_WHEEL_WRAP => launcher.wheel_wrap = checked,
                            _ => unreachable!(),
                        }
                        LRESULT(0)
                    }

                    IDC_SHORTCUT_KIND if code == CBN_SELCHANGE => {
                        win.update_shortcut_browse_button();
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_LIST if code == LBN_SELCHANGE => {
                        let index = SendMessageW(dlg_item(hwnd, IDC_SHORTCUT_LIST), LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                        if let Err(error) = win.save_shortcut_fields(false) {
                            win.populate_shortcut_list();
                            show_settings_error(hwnd, error);
                        } else {
                            win.select_shortcut(usize::try_from(index).ok());
                        }
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_NEW => {
                        win.select_shortcut(None);
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(
                            dlg_item(hwnd, IDC_SHORTCUT_NAME),
                        );
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_UPDATE => {
                        match win.save_shortcut_fields(true) {
                            Ok(()) => win.populate_shortcut_list(),
                            Err(error) => show_settings_error(hwnd, error),
                        }
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_DELETE => {
                        if let Some(index) = win.selected_shortcut_idx {
                            win.vm.config_draft.shortcuts.remove(index);
                            let len = win.vm.config_draft.shortcuts.len();
                            win.select_shortcut(len.checked_sub(1).map(|last| index.min(last)));
                        }
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_UP | IDC_SHORTCUT_DOWN => {
                        if let Some(index) = win.selected_shortcut_idx {
                            let to = if id == IDC_SHORTCUT_UP { index.checked_sub(1) } else { Some(index + 1) };
                            if let Some(to) = to.filter(|&i| i < win.vm.config_draft.shortcuts.len()) {
                                match win.save_shortcut_fields(true) {
                                    Ok(()) => {
                                        win.vm.config_draft.shortcuts.swap(index, to);
                                        win.select_shortcut(Some(to));
                                    }
                                    Err(error) => show_settings_error(hwnd, error),
                                }
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SHORTCUT_BROWSE => {
                        let kind = win.shortcut_kind();
                        if kind != ShortcutKind::Website {
                            match choose_shortcut_path(hwnd, kind) {
                                Ok(Some(path)) => {
                                    set_edit_text(hwnd, IDC_SHORTCUT_TARGET, &path);
                                    if edit_text(hwnd, IDC_SHORTCUT_NAME).trim().is_empty() {
                                        let name = std::path::Path::new(&path).file_name().and_then(|s| s.to_str()).unwrap_or(&path);
                                        set_edit_text(hwnd, IDC_SHORTCUT_NAME, name);
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => show_settings_error(hwnd, &error.to_string()),
                            }
                        }
                        LRESULT(0)
                    }

                    // Tab 3 Behavior
                    IDC_BEH_RESTORE_ALL => {
                        let _ = PostMessageW(
                            win.parent_hwnd,
                            0x8000 + 6, /* WM_APP_RESTORE_ALL */
                            WPARAM(0),
                            LPARAM(0),
                        );
                        let _ = MessageBoxW(hwnd, w!("すべての最小化ウィンドウを復元しました。"), w!("Kiri"), MB_OK | MB_ICONINFORMATION);
                        LRESULT(0)
                    }
                    IDC_BEH_OPEN_FOLDER => {
                        let dir = crate::diagnostics::default_app_data_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let dir_wide: Vec<u16> = dir
                            .as_os_str()
                            .encode_wide()
                            .chain(std::iter::once(0))
                            .collect();
                        let _ = windows::Win32::UI::Shell::ShellExecuteW(
                            hwnd,
                            w!("open"),
                            windows::core::PCWSTR(dir_wide.as_ptr()),
                            None,
                            None,
                            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                        );
                        LRESULT(0)
                    }
                    IDC_BEH_EXPORT => {
                        if let Err(e) = win.save_changes_to_config() {
                            show_settings_error(hwnd, &e);
                            return LRESULT(0);
                        }
                        match save_file_dialog(
                            hwnd,
                            "kiri-config.json",
                            "json",
                            &[("JSON ファイル (*.json)", "*.json")],
                        ) {
                            Ok(Some(path_str)) => {
                                let mgr = crate::config::ConfigManager::new(win.config_path.clone());
                                match mgr.export_config_to_path(
                                    &win.vm.config_draft,
                                    std::path::Path::new(&path_str),
                                ) {
                                    Ok(()) => {
                                        let _ = MessageBoxW(
                                            hwnd,
                                            w!("現在の設定をファイルへエクスポートしました。"),
                                            w!("Kiri 設定"),
                                            MB_OK | MB_ICONINFORMATION,
                                        );
                                    }
                                    Err(e) => show_settings_error(hwnd, &e),
                                }
                            }
                            Ok(None) => {}
                            Err(e) => show_settings_error(hwnd, &e.to_string()),
                        }
                        LRESULT(0)
                    }
                    IDC_BEH_IMPORT => {
                        match open_file_dialog(
                            hwnd,
                            false,
                            &[("JSON ファイル (*.json)", "*.json")],
                        ) {
                            Ok(Some(path_str)) => {
                                let mgr = crate::config::ConfigManager::new(win.config_path.clone());
                                match mgr.import_config_from_path(std::path::Path::new(&path_str)) {
                                    Ok((imported, warnings)) => {
                                        win.vm.config_draft = imported;
                                        win.vm.sync_binding_annotations();
                                        win.vm.sync_binding_rows();
                                        win.selected_scene_idx = Some(0);
                                        win.selected_app_idx = None;
                                        win.select_shortcut(None);
                                        win.populate_data();
                                        let msg = if warnings.is_empty() {
                                            "設定をインポートしました。\n[適用] または [OK] を押すと反映・保存されます。"
                                                .to_string()
                                        } else {
                                            format!(
                                                "設定をインポートしました（警告 {} 件あり）。\n[適用] または [OK] を押すと反映・保存されます。\n\n{}",
                                                warnings.len(),
                                                warnings.join("\n")
                                            )
                                        };
                                        let w_msg = windows::core::HSTRING::from(msg);
                                        let _ = MessageBoxW(
                                            hwnd,
                                            &w_msg,
                                            w!("Kiri 設定"),
                                            MB_OK | MB_ICONINFORMATION,
                                        );
                                    }
                                    Err(e) => show_settings_error(hwnd, &e),
                                }
                            }
                            Ok(None) => {}
                            Err(e) => show_settings_error(hwnd, &e.to_string()),
                        }
                        LRESULT(0)
                    }

                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }

            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }

            WM_DESTROY => {
                ACTIVE_SETTINGS_HWND.store(0, Ordering::Release);
                LRESULT(0)
            }

            WM_NCDESTROY => {
                // Free the heap-allocated SettingsWindow Box
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = Box::from_raw(ptr);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    result.unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

// ============================================================================
// "開いているウィンドウから追加" (Add Window) Modal Dialog
// ============================================================================

#[cfg(windows)]
struct AddWindowDialogData {
    parent_hwnd: windows::Win32::Foundation::HWND,
    parent_win_ptr: *mut SettingsWindow,
    selected_candidate_idx: Option<usize>,
    hfont: windows::Win32::Graphics::Gdi::HFONT,
    _dpi: u32,
}

#[cfg(windows)]
impl Drop for AddWindowDialogData {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        unsafe {
            if !self.hfont.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.hfont.0));
            }
        }
    }
}

#[cfg(windows)]
unsafe fn show_add_window_dialog(
    parent_hwnd: windows::Win32::Foundation::HWND,
    win: &mut SettingsWindow,
) {
    use std::sync::Once;
    use windows::core::w;
    use windows::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, GetWindowRect, LoadCursorW, RegisterClassW,
        SetForegroundWindow, SetWindowLongPtrW, ShowWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA,
        IDC_ARROW, SW_SHOW, WNDCLASSW, WS_CAPTION, WS_POPUP, WS_SYSMENU,
    };
    use crate::platform::monitors::dip_to_px;

    static REGISTER_ADDWIN_ONCE: Once = Once::new();
    let class_name = w!("KiriAddWindowDialogClass");
    let hinstance = GetModuleHandleW(None).unwrap();

    REGISTER_ADDWIN_ONCE.call_once(|| {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(add_window_wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut _),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
    });

    let dpi = win.dpi.max(72);
    let dlg_w = dip_to_px(680, dpi);
    let dlg_h = dip_to_px(480, dpi);

    let mut parent_rc = windows::Win32::Foundation::RECT::default();
    let (dlg_x, dlg_y) = if GetWindowRect(parent_hwnd, &mut parent_rc).is_ok() {
        let px = parent_rc.left + ((parent_rc.right - parent_rc.left) - dlg_w) / 2;
        let py = parent_rc.top + ((parent_rc.bottom - parent_rc.top) - dlg_h) / 2;
        (px.max(0), py.max(0))
    } else {
        (150, 150)
    };

    let hwnd = CreateWindowExW(
        windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
        class_name,
        w!("開いているウィンドウから追加"),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        dlg_x,
        dlg_y,
        dlg_w,
        dlg_h,
        parent_hwnd,
        None,
        hinstance,
        None,
    )
    .unwrap();

    let font = create_settings_ui_font(dpi);
    let state = Box::new(AddWindowDialogData {
        parent_hwnd,
        parent_win_ptr: win as *mut SettingsWindow,
        selected_candidate_idx: None,
        hfont: font,
        _dpi: dpi,
    });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);

    let _ = EnableWindow(parent_hwnd, false);
    init_add_window_controls(hwnd, hinstance, dpi, font);
    populate_add_window_data(hwnd);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

#[cfg(windows)]
unsafe fn init_add_window_controls(
    hwnd: windows::Win32::Foundation::HWND,
    hinstance: windows::Win32::Foundation::HMODULE,
    dpi: u32,
    font: windows::Win32::Graphics::Gdi::HFONT,
) {
    use crate::platform::monitors::dip_to_px;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        LVCFMT_LEFT, LVCF_FMT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
        LVM_INSERTCOLUMNW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_FULLROWSELECT, LVS_EX_GRIDLINES,
        LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, WC_LISTVIEWW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SendMessageW, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
        CBS_DROPDOWNLIST, ES_AUTOHSCROLL, WS_BORDER, WS_CHILD, WS_HSCROLL, WS_VISIBLE, WS_VSCROLL,
    };

    let create_c = |class: PCWSTR,
                    text: PCWSTR,
                    style_u32: u32,
                    x_dip: i32,
                    y_dip: i32,
                    w_dip: i32,
                    h_dip: i32,
                    id: usize|
     -> HWND {
        let h = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class,
            text,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                style_u32 | WS_CHILD.0 | WS_VISIBLE.0,
            ),
            dip_to_px(x_dip, dpi),
            dip_to_px(y_dip, dpi),
            dip_to_px(w_dip, dpi),
            dip_to_px(h_dip, dpi),
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _),
            hinstance,
            None,
        )
        .unwrap();
        let _ = SendMessageW(h, 0x0030, WPARAM(font.0 as usize), LPARAM(1));
        h
    };

    create_c(
        w!("STATIC"),
        w!("開いているウィンドウ一覧 (選択して追加):"),
        0,
        16,
        14,
        400,
        22,
        0,
    );

    let h_lv = create_c(
        WC_LISTVIEWW,
        w!(""),
        WS_BORDER.0
            | WS_VSCROLL.0
            | WS_HSCROLL.0
            | (LVS_REPORT as u32)
            | (LVS_SINGLESEL as u32)
            | (LVS_SHOWSELALWAYS as u32),
        16,
        38,
        648,
        240,
        IDC_ADDWIN_LIST,
    );
    let _ = SendMessageW(
        h_lv,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize),
    );

    // Columns
    let add_col = |idx: i32, title: &str, w_dip: i32| {
        let w_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT,
            fmt: LVCFMT_LEFT,
            cx: dip_to_px(w_dip, dpi),
            pszText: windows::core::PWSTR(w_title.as_ptr() as *mut u16),
            iSubItem: idx,
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_INSERTCOLUMNW,
            WPARAM(idx as usize),
            LPARAM(&col as *const _ as isize),
        );
    };
    add_col(0, "モニター", 75);
    add_col(1, "ウィンドウタイトル", 270);
    add_col(2, "EXE名", 140);
    add_col(3, "登録状況", 140);

    create_c(w!("STATIC"), w!("追加先シーン:"), 0, 16, 292, 100, 22, 0);
    create_c(
        w!("COMBOBOX"),
        w!(""),
        CBS_DROPDOWNLIST as u32,
        120,
        288,
        180,
        200,
        IDC_ADDWIN_SCENE_COMBO,
    );

    create_c(
        w!("BUTTON"),
        w!("常時表示にする"),
        BS_AUTOCHECKBOX as u32,
        320,
        290,
        140,
        24,
        IDC_ADDWIN_ALWAYS_VISIBLE,
    );

    create_c(
        w!("BUTTON"),
        w!("配置モニターで識別する"),
        BS_AUTOCHECKBOX as u32,
        470,
        290,
        194,
        24,
        IDC_ADDWIN_PIN_MONITOR,
    );

    create_c(
        w!("STATIC"),
        w!("再起動時の識別キーワード (空欄=タイトル不問):"),
        0,
        16,
        330,
        350,
        22,
        0,
    );
    create_c(
        w!("EDIT"),
        w!(""),
        WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
        16,
        354,
        648,
        28,
        IDC_ADDWIN_KEYWORD,
    );

    create_c(
        w!("STATIC"),
        w!("リストを選択またはダブルクリックで即座に追加できます"),
        0,
        16,
        401,
        420,
        28,
        IDC_ADDWIN_STATUS,
    );

    create_c(
        w!("BUTTON"),
        w!("追加"),
        BS_DEFPUSHBUTTON as u32,
        452,
        398,
        100,
        30,
        IDC_ADDWIN_BTN_ADD,
    );
    create_c(
        w!("BUTTON"),
        w!("閉じる"),
        BS_PUSHBUTTON as u32,
        564,
        398,
        100,
        30,
        IDC_ADDWIN_BTN_CLOSE,
    );
}

#[cfg(windows)]
unsafe fn populate_add_window_data(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTITEMW, LVM_SETITEMTEXTW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SendMessageW, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
        CB_SETCURSEL, GWLP_USERDATA,
    };

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddWindowDialogData;
    if ptr.is_null() {
        return;
    }
    let data = &mut *ptr;
    let parent_win = &*data.parent_win_ptr;

    let h_lv = dlg_item(hwnd, IDC_ADDWIN_LIST);
    let _ = SendMessageW(h_lv, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));

    for (idx, cand) in parent_win.vm.candidate_windows.iter().enumerate() {
        let mon = parent_win
            .vm
            .format_monitor_short_label(&cand.instance.monitor_id);
        let w_mon: Vec<u16> = mon.encode_utf16().chain(std::iter::once(0)).collect();
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 0,
            pszText: windows::core::PWSTR(w_mon.as_ptr() as *mut u16),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_INSERTITEMW,
            WPARAM(0),
            LPARAM(&mut item as *mut _ as isize),
        );

        let set_sub = |sub: i32, text: &str| {
            let w_text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sub_item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: idx as i32,
                iSubItem: sub,
                pszText: windows::core::PWSTR(w_text.as_ptr() as *mut u16),
                ..Default::default()
            };
            let _ = SendMessageW(
                h_lv,
                LVM_SETITEMTEXTW,
                WPARAM(idx),
                LPARAM(&mut sub_item as *mut _ as isize),
            );
        };

        let exe_name = std::path::Path::new(&cand.instance.exe_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&cand.instance.exe_path);
        let status = if cand.always_visible {
            "常時表示".to_string()
        } else if cand.bound_scene_ids.is_empty() {
            "未登録".to_string()
        } else {
            let names: Vec<String> = cand
                .bound_scene_ids
                .iter()
                .filter_map(|sid| {
                    parent_win
                        .vm
                        .config_draft
                        .scenes
                        .iter()
                        .find(|s| &s.id == sid)
                        .map(|s| s.name.clone())
                })
                .collect();
            if names.is_empty() {
                "登録済".to_string()
            } else {
                names.join(", ")
            }
        };

        set_sub(1, &cand.instance.title);
        set_sub(2, exe_name);
        set_sub(3, &status);
    }

    let h_combo = dlg_item(hwnd, IDC_ADDWIN_SCENE_COMBO);
    let prev_sel = SendMessageW(h_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let _ = SendMessageW(h_combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for scene in &parent_win.vm.config_draft.scenes {
        let w_name: Vec<u16> = scene.name.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = SendMessageW(
            h_combo,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(w_name.as_ptr() as isize),
        );
    }
    if !parent_win.vm.config_draft.scenes.is_empty() {
        let default_sel = if prev_sel >= 0
            && (prev_sel as usize) < parent_win.vm.config_draft.scenes.len()
        {
            prev_sel as usize
        } else {
            parent_win
                .selected_scene_idx
                .unwrap_or(0)
                .min(parent_win.vm.config_draft.scenes.len() - 1)
        };
        let _ = SendMessageW(h_combo, CB_SETCURSEL, WPARAM(default_sel), LPARAM(0));
    }
}

#[cfg(windows)]
unsafe fn load_add_window_candidate_details(hwnd: windows::Win32::Foundation::HWND, sel_idx: usize) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SendMessageW, BM_SETCHECK, GWLP_USERDATA};

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddWindowDialogData;
    if ptr.is_null() {
        return;
    }
    let data = &*ptr;
    let parent_win = &*data.parent_win_ptr;
    let Some(cand) = parent_win.vm.candidate_windows.get(sel_idx) else {
        return;
    };

    let _ = SendMessageW(
        dlg_item(hwnd, IDC_ADDWIN_ALWAYS_VISIBLE),
        BM_SETCHECK,
        WPARAM(if cand.always_visible { BST_CHECKED } else { BST_UNCHECKED } as usize),
        LPARAM(0),
    );
    let _ = SendMessageW(
        dlg_item(hwnd, IDC_ADDWIN_PIN_MONITOR),
        BM_SETCHECK,
        WPARAM(BST_UNCHECKED as usize),
        LPARAM(0),
    );

    let suggested = parent_win
        .vm
        .suggest_default_title_keyword(&cand.instance)
        .unwrap_or_default();
    set_edit_text(hwnd, IDC_ADDWIN_KEYWORD, &suggested);
}

#[cfg(windows)]
unsafe fn add_selected_candidate_window(hwnd: windows::Win32::Foundation::HWND, data: &mut AddWindowDialogData) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, SendMessageW, BM_GETCHECK, CB_GETCURSEL};

    let parent_win = &mut *data.parent_win_ptr;
    let Some(cand_idx) = data.selected_candidate_idx else {
        set_edit_text(hwnd, IDC_ADDWIN_STATUS, "⚠️ 先にウィンドウを一覧から選択してください。");
        return;
    };
    let Some(cand) = parent_win.vm.candidate_windows.get(cand_idx) else {
        return;
    };
    let h_combo = dlg_item(hwnd, IDC_ADDWIN_SCENE_COMBO);
    let s_sel = SendMessageW(h_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
    let Some(scene) = parent_win.vm.config_draft.scenes.get(s_sel) else {
        return;
    };
    let scene_id = scene.id.clone();
    let scene_name = scene.name.clone();
    let h_av = dlg_item(hwnd, IDC_ADDWIN_ALWAYS_VISIBLE);
    let always_visible =
        SendMessageW(h_av, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize;

    let h_pin = dlg_item(hwnd, IDC_ADDWIN_PIN_MONITOR);
    let pin_mon = SendMessageW(h_pin, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize;

    let h_kw = dlg_item(hwnd, IDC_ADDWIN_KEYWORD);
    let mut buf = [0u16; 256];
    let len = GetWindowTextW(h_kw, &mut buf);
    let custom_kw = if len > 0 {
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        None
    };

    let instance = cand.instance.clone();
    let exe_name = std::path::Path::new(&instance.exe_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&instance.exe_path)
        .to_string();

    parent_win.vm.bind_window_to_scene_with_options(
        &instance,
        &scene_id,
        always_visible,
        custom_kw,
        pin_mon,
    );
    parent_win.populate_data();
    populate_add_window_data(hwnd);

    set_edit_text(
        hwnd,
        IDC_ADDWIN_STATUS,
        &format!(
            "✓ 「{}」をシーン「{}」へ追加しました。",
            exe_name, scene_name
        ),
    );
}

#[cfg(windows)]
unsafe extern "system" fn add_window_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::Controls::{LVN_ITEMCHANGED, NMHDR, NMITEMACTIVATE, NMLISTVIEW};
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW,
        GWLP_USERDATA, WM_CLOSE, WM_COMMAND, WM_NCDESTROY, WM_NOTIFY,
    };

    let res = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddWindowDialogData;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let data = &mut *ptr;

        match msg {
            WM_NOTIFY => {
                let nmhdr = &*(lparam.0 as *const NMHDR);
                if nmhdr.idFrom == IDC_ADDWIN_LIST && nmhdr.code == LVN_ITEMCHANGED {
                    let nmlv = &*(lparam.0 as *const NMLISTVIEW);
                    if (nmlv.uNewState & windows::Win32::UI::Controls::LVIS_SELECTED.0) != 0
                        && nmlv.iItem >= 0
                    {
                        let sel = nmlv.iItem as usize;
                        data.selected_candidate_idx = Some(sel);
                        load_add_window_candidate_details(hwnd, sel);
                    }
                } else if nmhdr.idFrom == IDC_ADDWIN_LIST && nmhdr.code == NM_DBLCLK {
                    let act = &*(lparam.0 as *const NMITEMACTIVATE);
                    if act.iItem >= 0 {
                        data.selected_candidate_idx = Some(act.iItem as usize);
                        load_add_window_candidate_details(hwnd, act.iItem as usize);
                        add_selected_candidate_window(hwnd, data);
                    }
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as usize;
                match id {
                    IDC_ADDWIN_BTN_ADD => {
                        add_selected_candidate_window(hwnd, data);
                        LRESULT(0)
                    }

                    IDC_ADDWIN_BTN_CLOSE => {
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }

                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }

            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }

            WM_NCDESTROY => {
                let parent_hwnd = data.parent_hwnd;
                let _ = EnableWindow(parent_hwnd, true);
                let _ = SetForegroundWindow(parent_hwnd);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = Box::from_raw(ptr);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    res.unwrap_or_else(|_| DefWindowProcW(hwnd, msg, wparam, lparam))
}

// ============================================================================
// Ambiguous Candidate Disambiguation Picker Modal Dialog
// ============================================================================

#[cfg(windows)]
struct PickCandidateDialogData {
    parent_hwnd: windows::Win32::Foundation::HWND,
    parent_win_ptr: *mut SettingsWindow,
    binding_id: String,
    candidates: Vec<crate::platform::WindowInstance>,
    hfont: windows::Win32::Graphics::Gdi::HFONT,
}

#[cfg(windows)]
impl Drop for PickCandidateDialogData {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        unsafe {
            if !self.hfont.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(self.hfont.0));
            }
        }
    }
}

#[cfg(windows)]
unsafe fn show_pick_candidate_dialog(
    parent_hwnd: windows::Win32::Foundation::HWND,
    win: &mut SettingsWindow,
) {
    use crate::platform::monitors::dip_to_px;
    use std::sync::Once;
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Controls::{
        LVCFMT_LEFT, LVCF_FMT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT,
        LVITEMW, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
        LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVIS_FOCUSED, LVIS_SELECTED, LVS_EX_FULLROWSELECT,
        LVS_EX_GRIDLINES, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, WC_LISTVIEWW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, GetWindowRect, LoadCursorW, RegisterClassW, SendMessageW,
        SetForegroundWindow, SetWindowLongPtrW, ShowWindow, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
        CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, SW_SHOW, WNDCLASSW, WS_BORDER,
        WS_CAPTION, WS_CHILD, WS_HSCROLL, WS_POPUP, WS_SYSMENU, WS_VISIBLE, WS_VSCROLL,
    };

    let Some(app_idx) = win.selected_app_idx else {
        return;
    };
    let Some(row) = win.vm.binding_rows.get(app_idx) else {
        return;
    };
    if row.ambiguous_candidates.is_empty() {
        return;
    }

    static REGISTER_PICK_ONCE: Once = Once::new();
    let class_name = w!("KiriPickCandidateDialogClass");
    let hinstance = GetModuleHandleW(None).unwrap();

    REGISTER_PICK_ONCE.call_once(|| {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(pick_candidate_wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut _),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
    });

    let dpi = win.dpi.max(72);
    let dlg_w = dip_to_px(620, dpi);
    let dlg_h = dip_to_px(360, dpi);

    let mut parent_rc = windows::Win32::Foundation::RECT::default();
    let (dlg_x, dlg_y) = if GetWindowRect(parent_hwnd, &mut parent_rc).is_ok() {
        let px = parent_rc.left + ((parent_rc.right - parent_rc.left) - dlg_w) / 2;
        let py = parent_rc.top + ((parent_rc.bottom - parent_rc.top) - dlg_h) / 2;
        (px.max(0), py.max(0))
    } else {
        (160, 160)
    };

    let hwnd = CreateWindowExW(
        windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
        class_name,
        w!("複数候補から対象ウィンドウを特定"),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        dlg_x,
        dlg_y,
        dlg_w,
        dlg_h,
        parent_hwnd,
        None,
        hinstance,
        None,
    )
    .unwrap();

    let font = create_settings_ui_font(dpi);
    let candidates = row.ambiguous_candidates.clone();
    let binding_id = row.binding_id.clone();

    let create_c = |class: windows::core::PCWSTR,
                    text: windows::core::PCWSTR,
                    style_u32: u32,
                    x_dip: i32,
                    y_dip: i32,
                    w_dip: i32,
                    h_dip: i32,
                    id: usize|
     -> HWND {
        let h = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class,
            text,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                style_u32 | WS_CHILD.0 | WS_VISIBLE.0,
            ),
            dip_to_px(x_dip, dpi),
            dip_to_px(y_dip, dpi),
            dip_to_px(w_dip, dpi),
            dip_to_px(h_dip, dpi),
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _),
            hinstance,
            None,
        )
        .unwrap();
        let _ = SendMessageW(h, 0x0030, WPARAM(font.0 as usize), LPARAM(1));
        h
    };

    create_c(
        w!("STATIC"),
        w!("一致するウィンドウが複数見つかりました。このルールに紐付ける1つを選択してください:"),
        0,
        14,
        12,
        590,
        22,
        0,
    );
    let h_lv = create_c(
        WC_LISTVIEWW,
        w!(""),
        WS_BORDER.0
            | WS_VSCROLL.0
            | WS_HSCROLL.0
            | (LVS_REPORT as u32)
            | (LVS_SINGLESEL as u32)
            | (LVS_SHOWSELALWAYS as u32),
        14,
        38,
        590,
        220,
        IDC_PICKCAND_LIST,
    );
    let _ = SendMessageW(
        h_lv,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize),
    );
    let add_col = |idx: i32, title: &str, w_dip: i32| {
        let w_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT,
            fmt: LVCFMT_LEFT,
            cx: dip_to_px(w_dip, dpi),
            pszText: windows::core::PWSTR(w_title.as_ptr() as *mut u16),
            iSubItem: idx,
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_INSERTCOLUMNW,
            WPARAM(idx as usize),
            LPARAM(&col as *const _ as isize),
        );
    };
    add_col(0, "モニター", 90);
    add_col(1, "HWND", 95);
    add_col(2, "PID", 65);
    add_col(3, "タイトル", 320);

    for (idx, c) in candidates.iter().enumerate() {
        let mon_label = win.vm.format_monitor_short_label(&c.monitor_id);
        let w_mon: Vec<u16> = mon_label.encode_utf16().chain(std::iter::once(0)).collect();
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 0,
            pszText: windows::core::PWSTR(w_mon.as_ptr() as *mut u16),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_INSERTITEMW,
            WPARAM(0),
            LPARAM(&mut item as *mut _ as isize),
        );

        let set_sub = |sub: i32, val: &str| {
            let w_val: Vec<u16> = val.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sub_it = LVITEMW {
                mask: LVIF_TEXT,
                iItem: idx as i32,
                iSubItem: sub,
                pszText: windows::core::PWSTR(w_val.as_ptr() as *mut u16),
                ..Default::default()
            };
            let _ = SendMessageW(
                h_lv,
                LVM_SETITEMTEXTW,
                WPARAM(idx),
                LPARAM(&mut sub_it as *mut _ as isize),
            );
        };
        set_sub(1, &format!("0x{:X}", c.hwnd));
        set_sub(2, &c.pid.to_string());
        let title = if c.title.is_empty() {
            "(タイトルなし)"
        } else {
            &c.title
        };
        set_sub(3, title);
    }

    let mut sel_state = LVITEMW {
        state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        ..Default::default()
    };
    let _ = SendMessageW(
        h_lv,
        LVM_SETITEMSTATE,
        WPARAM(0),
        LPARAM(&mut sel_state as *mut _ as isize),
    );

    create_c(
        w!("BUTTON"),
        w!("このウィンドウに確定"),
        BS_DEFPUSHBUTTON as u32,
        344,
        274,
        150,
        34,
        IDC_PICKCAND_BTN_OK,
    );
    create_c(
        w!("BUTTON"),
        w!("キャンセル"),
        BS_PUSHBUTTON as u32,
        504,
        274,
        100,
        34,
        IDC_PICKCAND_BTN_CANCEL,
    );

    let data = Box::new(PickCandidateDialogData {
        parent_hwnd,
        parent_win_ptr: win as *mut SettingsWindow,
        binding_id,
        candidates,
        hfont: font,
    });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(data) as isize);

    let _ = EnableWindow(parent_hwnd, false);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

#[cfg(windows)]
unsafe extern "system" fn pick_candidate_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Controls::{
        LVM_GETNEXTITEM, LVNI_SELECTED, NMHDR, NMITEMACTIVATE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetWindowLongPtrW, SendMessageW, SetForegroundWindow,
        SetWindowLongPtrW, GWLP_USERDATA, WM_CLOSE, WM_COMMAND, WM_NCDESTROY, WM_NOTIFY,
    };

    let res = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PickCandidateDialogData;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let data = &mut *ptr;

        let commit_idx = |idx: i32| {
            if idx >= 0 {
                if let Some(chosen) = data.candidates.get(idx as usize).cloned() {
                    let parent_win = &mut *data.parent_win_ptr;
                    parent_win
                        .vm
                        .resolve_binding_to_instance(&data.binding_id, &chosen);
                    parent_win.populate_data();
                }
            }
            let _ = DestroyWindow(hwnd);
        };

        match msg {
            WM_NOTIFY => {
                let nmhdr = &*(lparam.0 as *const NMHDR);
                if nmhdr.idFrom == IDC_PICKCAND_LIST && nmhdr.code == NM_DBLCLK {
                    let act = &*(lparam.0 as *const NMITEMACTIVATE);
                    commit_idx(act.iItem);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as usize;
                match id {
                    IDC_PICKCAND_BTN_OK => {
                        let sel = SendMessageW(
                            dlg_item(hwnd, IDC_PICKCAND_LIST),
                            LVM_GETNEXTITEM,
                            WPARAM(usize::MAX),
                            LPARAM(LVNI_SELECTED as isize),
                        )
                        .0 as i32;
                        commit_idx(sel);
                        LRESULT(0)
                    }
                    IDC_PICKCAND_BTN_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let parent_hwnd = data.parent_hwnd;
                let _ = EnableWindow(parent_hwnd, true);
                let _ = SetForegroundWindow(parent_hwnd);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = Box::from_raw(ptr);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    });

    res.unwrap_or_else(|_| DefWindowProcW(hwnd, msg, wparam, lparam))
}
