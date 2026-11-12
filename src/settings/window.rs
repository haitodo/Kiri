//! Win32 Settings Window implementation.
//!
//! Provides an on-demand dialog window with 4 tabs:
//! 1. Scenes: Add, remove, rename, order, icon, and pinned status
//! 2. Windows: Enumerate open candidate windows, warnings, and scene binding
//! 3. Launcher: Orientation, button/grip size, visible slots, topmost, snap, focus
//! 4. Behavior: Target monitor selector, Windows autostart, verbose logging, full restore

use crate::app::AppController;
use crate::config::{MonitorPolicy, Orientation, Scene};
use crate::settings::{set_autostart_enabled, SettingsViewModel};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(windows)]
use windows::Win32::UI::Controls::{
    LVN_ITEMCHANGED, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_FULLROWSELECT, LVS_EX_GRIDLINES,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMITEMACTIVATE, NMLISTVIEW, WC_LISTVIEWW,
};

pub const WM_APP_SETTINGS_SAVED: u32 = 0x8000 + 5;

static ACTIVE_SETTINGS_HWND: AtomicUsize = AtomicUsize::new(0);

const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

const NM_CLICK: u32 = 0u32.wrapping_sub(2); // 0xFFFFFFFE

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

// Tab 1 (Apps & Scenes Matrix)
const IDC_APP_MATRIX_LIST: usize = 301;
const IDC_APP_ADD_WINDOW: usize = 302;
const IDC_APP_DELETE: usize = 303;
const IDC_APP_REFRESH: usize = 304;
const IDC_APP_TITLE_KEYWORD: usize = 305;
const IDC_APP_MATCH_MONITOR: usize = 306;
const IDC_APP_APPLY_RULE: usize = 307;
const IDC_APP_DETAILS: usize = 308;

// Tab 2 (Launcher)
const IDC_LCH_ORIENT_COMBO: usize = 401;
const IDC_LCH_BTN_SIZE: usize = 402;
const IDC_LCH_GRIP_SIZE: usize = 403;
const IDC_LCH_SLOTS: usize = 404;
const IDC_LCH_TOPMOST: usize = 405;
const IDC_LCH_SNAP: usize = 406;
const IDC_LCH_FOCUS: usize = 407;
const IDC_LCH_RESET_POS: usize = 408;

// Tab 3 (Behavior)
const IDC_BEH_MONITOR_COMBO: usize = 501;
const IDC_BEH_AUTOSTART: usize = 502;
const IDC_BEH_VERBOSE_TITLE: usize = 503;
const IDC_BEH_RESTORE_ALL: usize = 504;
const IDC_BEH_OPEN_FOLDER: usize = 505;
const IDC_BEH_RESTORE_ALL_ON_SWITCH: usize = 506;

// Add Window Subdialog IDs
const IDC_ADDWIN_LIST: usize = 601;
const IDC_ADDWIN_SCENE_COMBO: usize = 602;
const IDC_ADDWIN_ALWAYS_VISIBLE: usize = 603;
const IDC_ADDWIN_PIN_MONITOR: usize = 604;
const IDC_ADDWIN_KEYWORD: usize = 605;
const IDC_ADDWIN_BTN_ADD: usize = 606;
const IDC_ADDWIN_BTN_CLOSE: usize = 607;

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
    pub tab_controls: [Vec<windows::Win32::Foundation::HWND>; 4],
    pub control_layouts: Vec<ControlDipLayout>,
    pub hfont: windows::Win32::Graphics::Gdi::HFONT,
    pub dpi: u32,
    pub applying_dpi: bool,
}

#[cfg(windows)]
impl Drop for SettingsWindow {
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
        w!("Yu Gothic UI"),
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
    pub fn show(parent_hwnd: windows::Win32::Foundation::HWND, app: &AppController) {
        use std::sync::Once;
        use windows::core::w;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::Controls::{
            InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
            INITCOMMONCONTROLSEX,
        };
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, IsIconic, LoadCursorW, RegisterClassW, SetForegroundWindow,
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

            let mut win = Box::new(Self {
                hwnd,
                parent_hwnd,
                vm,
                selected_scene_idx: None,
                selected_app_idx: None,
                tab_controls: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                control_layouts: Vec::with_capacity(48),
                hfont: windows::Win32::Graphics::Gdi::HFONT::default(),
                dpi: actual_win_dpi,
                applying_dpi: false,
            });

            let ptr = &mut *win as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            win.init_controls(hinstance);
            win.clamp_to_current_monitor_work_area();

            // Box is intentionally leaked while the window is alive; dropped in WM_NCDESTROY
            let _ = Box::into_raw(win);

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    unsafe fn init_controls(&mut self, hinstance: windows::Win32::Foundation::HMODULE) {
        use crate::platform::monitors::dip_to_px;
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::Controls::{TCIF_TEXT, TCITEMW, TCM_INSERTITEMW, WC_TABCONTROLW};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, SendMessageW, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST,
            ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, WS_BORDER,
            WS_CHILD, WS_HSCROLL, WS_VISIBLE, WS_VSCROLL,
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

        let tab_titles = [w!("シーン"), w!("アプリとシーン"), w!("ランチャー"), w!("動作")];
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
            260,
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
            260,
            360,
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
            88,
            32,
            IDC_SCENE_ADD,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("削除"),
            BS_PUSHBUTTON as u32,
            120,
            442,
            72,
            32,
            IDC_SCENE_DEL,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("↑"),
            BS_PUSHBUTTON as u32,
            196,
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
            244,
            442,
            44,
            32,
            IDC_SCENE_DOWN,
            ControlKind::Standard,
        ));

        // Right details area
        t0.push(create_ctrl(
            w!("STATIC"),
            w!("シーンID:"),
            0,
            308,
            48,
            140,
            20,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_READONLY as u32),
            308,
            70,
            150,
            28,
            IDC_SCENE_ID,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("表示名:"),
            0,
            470,
            48,
            140,
            20,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            470,
            70,
            170,
            28,
            IDC_SCENE_NAME,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("名前を更新"),
            BS_PUSHBUTTON as u32,
            650,
            70,
            138,
            28,
            IDC_SCENE_UPDATE,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("アイコン (chart / code / globe または .ico パス):"),
            0,
            308,
            106,
            480,
            20,
            0,
            ControlKind::Standard,
        ));
        t0.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_AUTOHSCROLL as u32),
            308,
            128,
            480,
            28,
            IDC_SCENE_ICON,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("ランチャーに固定表示する (pinned)"),
            BS_AUTOCHECKBOX as u32,
            308,
            166,
            320,
            26,
            IDC_SCENE_PINNED,
            ControlKind::Standard,
        ));

        t0.push(create_ctrl(
            w!("STATIC"),
            w!("このシーンに登録されているアプリ:"),
            0,
            308,
            202,
            480,
            20,
            0,
            ControlKind::Standard,
        ));
        let scene_apps_list = create_ctrl(
            w!("LISTBOX"),
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (LBS_NOTIFY as u32) | (LBS_NOINTEGRALHEIGHT as u32),
            308,
            226,
            480,
            208,
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
            170,
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
            764,
            76,
            IDC_APP_DETAILS,
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
            w!("常時表示件数 (visible_slots):"),
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
            w!("常時最前面に表示する (always_on_top)"),
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
            w!("画面端にスナップする (snap_to_edges, 12 DIP以内)"),
            BS_AUTOCHECKBOX as u32,
            32,
            272,
            520,
            28,
            IDC_LCH_SNAP,
            ControlKind::Standard,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("シーン切替時に最前面ウィンドウへフォーカスを移動する (focus_on_switch)"),
            BS_AUTOCHECKBOX as u32,
            32,
            310,
            660,
            28,
            IDC_LCH_FOCUS,
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
            w!("Windows ログオン時に自動起動する (HKCU\\Run)"),
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
            240,
            34,
            IDC_BEH_OPEN_FOLDER,
            ControlKind::Standard,
        ));
        self.tab_controls[3] = t3;
        self.control_layouts = layouts;

        self.apply_dpi_and_layout(dpi);
        self.populate_data();
        self.switch_tab(0);
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

        if let Some(scene) = self.vm.config_draft.scenes.get(idx) {
            unsafe {
                let set_txt = |id: usize, val: &str| {
                    let h = dlg_item(self.hwnd, id);
                    let w: Vec<u16> = val.encode_utf16().chain(std::iter::once(0)).collect();
                    let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
                };
                set_txt(IDC_SCENE_ID, &scene.id);
                set_txt(IDC_SCENE_NAME, &scene.name);
                set_txt(IDC_SCENE_ICON, &scene.icon);

                let h_chk = dlg_item(self.hwnd, IDC_SCENE_PINNED);
                let st = if scene.pinned { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h_chk, BM_SETCHECK, WPARAM(st), LPARAM(0));

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
    }

    pub fn save_current_scene_fields(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, SendMessageW, BM_GETCHECK};

        if let Some(idx) = self.selected_scene_idx {
            if let Some(scene) = self.vm.config_draft.scenes.get_mut(idx) {
                unsafe {
                    let get_text = |ctrl_id: usize| -> String {
                        let h = dlg_item(self.hwnd, ctrl_id);
                        let mut buf = [0u16; 256];
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
                }
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
            insert_col(0, "状態", 74, LVCFMT_LEFT);
            // Col 1: アプリ / ウィンドウ
            insert_col(1, "アプリ / ウィンドウ", 240, LVCFMT_LEFT);

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
                let warn_sym = if !row.warnings.is_empty() { " ⚠️" } else { "" };
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
                    }
                }
                details.push_str("\r\n");
            }

            unsafe {
                let h_det = dlg_item(self.hwnd, IDC_APP_DETAILS);
                let w: Vec<u16> = details.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(h_det, PCWSTR(w.as_ptr()));

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
        }
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
            // Read Launcher controls
            let h_orient = dlg_item(self.hwnd, IDC_LCH_ORIENT_COMBO);
            let orient_sel = SendMessageW(h_orient, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
            self.vm.config_draft.launcher.orientation = if orient_sel == 1 {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            };

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

            let get_chk = |id: usize| -> bool {
                let h = dlg_item(self.hwnd, id);
                SendMessageW(h, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize
            };
            self.vm.config_draft.launcher.always_on_top = get_chk(IDC_LCH_TOPMOST);
            self.vm.config_draft.launcher.snap_to_edges = get_chk(IDC_LCH_SNAP);
            self.vm.config_draft.launcher.focus_on_switch = get_chk(IDC_LCH_FOCUS);
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
        SendMessageW, SetWindowLongPtrW, GWLP_USERDATA, LBN_SELCHANGE, LB_GETCURSEL,
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
                    IDC_BTN_OK => {
                        let _ = win.save_changes_to_config();
                        let _ = PostMessageW(
                            win.parent_hwnd,
                            WM_APP_SETTINGS_SAVED,
                            WPARAM(Box::into_raw(Box::new(win.vm.config_draft.clone())) as usize),
                            LPARAM(0),
                        );
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    IDC_BTN_APPLY => {
                        let _ = win.save_changes_to_config();
                        let _ = PostMessageW(
                            win.parent_hwnd,
                            WM_APP_SETTINGS_SAVED,
                            WPARAM(Box::into_raw(Box::new(win.vm.config_draft.clone())) as usize),
                            LPARAM(0),
                        );
                        LRESULT(0)
                    }
                    IDC_BTN_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }

                    // Tab 0 Scene selection changed
                    IDC_SCENE_LIST if code == LBN_SELCHANGE => {
                        win.save_current_scene_fields();
                        let h = dlg_item(hwnd, IDC_SCENE_LIST);
                        let sel = SendMessageW(h, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                        win.selected_scene_idx = Some(sel);
                        win.load_selected_scene_details(sel);
                        LRESULT(0)
                    }
                    IDC_SCENE_ADD => {
                        win.save_current_scene_fields();
                        let new_id = format!("scene-{}", win.vm.config_draft.scenes.len() + 1);
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

                    // Tab 2 Reset position
                    IDC_LCH_RESET_POS => {
                        win.vm.config_draft.launcher.position = None;
                        let _ = MessageBoxW(hwnd, w!("ランチャー位置を初期化しました。[適用] または [OK] で反映されます。"), w!("Kiri"), MB_OK | MB_ICONINFORMATION);
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
        w!("BUTTON"),
        w!("追加"),
        BS_DEFPUSHBUTTON as u32,
        450,
        396,
        100,
        34,
        IDC_ADDWIN_BTN_ADD,
    );
    create_c(
        w!("BUTTON"),
        w!("閉じる"),
        BS_PUSHBUTTON as u32,
        564,
        396,
        100,
        34,
        IDC_ADDWIN_BTN_CLOSE,
    );
}

#[cfg(windows)]
unsafe fn populate_add_window_data(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        LVIF_PARAM, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTITEMW, LVM_SETITEMSTATE,
        LVM_SETITEMTEXTW, LVIS_FOCUSED, LVIS_SELECTED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SendMessageW, BM_SETCHECK, CB_ADDSTRING, CB_RESETCONTENT, CB_SETCURSEL,
        GWLP_USERDATA,
    };

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddWindowDialogData;
    if ptr.is_null() {
        return;
    }
    let data = &mut *ptr;
    let parent_win = &mut *data.parent_win_ptr;

    let h_lv = dlg_item(hwnd, IDC_ADDWIN_LIST);
    let _ = SendMessageW(h_lv, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));

    for (idx, cand) in parent_win.vm.candidate_windows.iter().enumerate() {
        let mon_label = parent_win.vm.format_monitor_short_label(&cand.instance.monitor_id);
        let w_mon: Vec<u16> = mon_label.encode_utf16().chain(std::iter::once(0)).collect();

        let mut item = LVITEMW {
            mask: LVIF_TEXT | LVIF_PARAM,
            iItem: idx as i32,
            iSubItem: 0,
            pszText: windows::core::PWSTR(w_mon.as_ptr() as *mut u16),
            lParam: LPARAM(idx as isize),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_INSERTITEMW,
            WPARAM(0),
            LPARAM(&mut item as *mut _ as isize),
        );

        let title_str = if cand.instance.title.is_empty() {
            "(タイトルなし)".to_string()
        } else {
            cand.instance.title.clone()
        };
        let w_title: Vec<u16> = title_str.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sub_title = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 1,
            pszText: windows::core::PWSTR(w_title.as_ptr() as *mut u16),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_SETITEMTEXTW,
            WPARAM(idx),
            LPARAM(&mut sub_title as *mut _ as isize),
        );

        let exe_name = std::path::Path::new(&cand.instance.exe_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&cand.instance.exe_path);
        let w_exe: Vec<u16> = exe_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sub_exe = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 2,
            pszText: windows::core::PWSTR(w_exe.as_ptr() as *mut u16),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_SETITEMTEXTW,
            WPARAM(idx),
            LPARAM(&mut sub_exe as *mut _ as isize),
        );

        let status_str = if cand.bound_scene_ids.is_empty() {
            "未登録".to_string()
        } else {
            let scene_names: Vec<&str> = parent_win
                .vm
                .config_draft
                .scenes
                .iter()
                .filter(|s| cand.bound_scene_ids.iter().any(|b| b == &s.id))
                .map(|s| s.name.as_str())
                .collect();
            format!("登録済 ({})", scene_names.join(","))
        };
        let w_status: Vec<u16> = status_str.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sub_status = LVITEMW {
            mask: LVIF_TEXT,
            iItem: idx as i32,
            iSubItem: 3,
            pszText: windows::core::PWSTR(w_status.as_ptr() as *mut u16),
            ..Default::default()
        };
        let _ = SendMessageW(
            h_lv,
            LVM_SETITEMTEXTW,
            WPARAM(idx),
            LPARAM(&mut sub_status as *mut _ as isize),
        );
    }

    // Populate scene combobox
    let h_combo = dlg_item(hwnd, IDC_ADDWIN_SCENE_COMBO);
    let _ = SendMessageW(h_combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for scene in &parent_win.vm.config_draft.scenes {
        let label = format!("{} [{}]", scene.name, scene.id);
        let w_label: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = SendMessageW(
            h_combo,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(w_label.as_ptr() as isize),
        );
    }
    let _ = SendMessageW(h_combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));

    // Default checkboxes
    let h_pin = dlg_item(hwnd, IDC_ADDWIN_PIN_MONITOR);
    let _ = SendMessageW(h_pin, BM_SETCHECK, WPARAM(BST_CHECKED), LPARAM(0));

    let h_av = dlg_item(hwnd, IDC_ADDWIN_ALWAYS_VISIBLE);
    let _ = SendMessageW(h_av, BM_SETCHECK, WPARAM(BST_UNCHECKED), LPARAM(0));

    if !parent_win.vm.candidate_windows.is_empty() {
        data.selected_candidate_idx = Some(0);
        let mut state_item = LVITEMW {
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
            LPARAM(&mut state_item as *mut _ as isize),
        );
        load_add_window_candidate_details(hwnd, 0);
    }
}

#[cfg(windows)]
unsafe fn load_add_window_candidate_details(hwnd: windows::Win32::Foundation::HWND, idx: usize) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowTextW, GWLP_USERDATA};

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddWindowDialogData;
    if ptr.is_null() {
        return;
    }
    let data = &mut *ptr;
    let parent_win = &mut *data.parent_win_ptr;

    if let Some(cand) = parent_win.vm.candidate_windows.get(idx) {
        let suggested_kw = parent_win
            .vm
            .suggest_default_title_keyword(&cand.instance)
            .unwrap_or_default();
        let h_kw = dlg_item(hwnd, IDC_ADDWIN_KEYWORD);
        let w_kw: Vec<u16> = suggested_kw.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = SetWindowTextW(h_kw, PCWSTR(w_kw.as_ptr()));
    }
}

#[cfg(windows)]
unsafe extern "system" fn add_window_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::core::w;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Controls::{LVN_ITEMCHANGED, NMHDR, NMLISTVIEW};
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowTextW,
        MessageBoxW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, BM_GETCHECK,
        CB_GETCURSEL, GWLP_USERDATA, MB_ICONINFORMATION, MB_OK, WM_CLOSE, WM_COMMAND,
        WM_NCDESTROY, WM_NOTIFY,
    };

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
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            match id {
                IDC_ADDWIN_BTN_ADD => {
                    let parent_win = &mut *data.parent_win_ptr;
                    if let Some(cand_idx) = data.selected_candidate_idx {
                        if let Some(cand) = parent_win.vm.candidate_windows.get(cand_idx) {
                            let h_combo = dlg_item(hwnd, IDC_ADDWIN_SCENE_COMBO);
                            let s_sel = SendMessageW(h_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0
                                as usize;
                            if let Some(scene) = parent_win.vm.config_draft.scenes.get(s_sel) {
                                let scene_id = scene.id.clone();
                                let h_av = dlg_item(hwnd, IDC_ADDWIN_ALWAYS_VISIBLE);
                                let always_visible =
                                    SendMessageW(h_av, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                                        == BST_CHECKED as isize;

                                let h_pin = dlg_item(hwnd, IDC_ADDWIN_PIN_MONITOR);
                                let pin_mon =
                                    SendMessageW(h_pin, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                                        == BST_CHECKED as isize;

                                let h_kw = dlg_item(hwnd, IDC_ADDWIN_KEYWORD);
                                let mut buf = [0u16; 256];
                                let len = GetWindowTextW(h_kw, &mut buf);
                                let custom_kw = if len > 0 {
                                    Some(String::from_utf16_lossy(&buf[..len as usize]))
                                } else {
                                    None
                                };

                                let instance = cand.instance.clone();
                                parent_win.vm.bind_window_to_scene_with_options(
                                    &instance,
                                    &scene_id,
                                    always_visible,
                                    custom_kw,
                                    pin_mon,
                                );
                                parent_win.populate_data();
                                populate_add_window_data(hwnd);

                                let _ = MessageBoxW(
                                    hwnd,
                                    w!("選択したウィンドウを登録しました。続けて他のウィンドウを追加するか、閉じてください。"),
                                    w!("Kiri"),
                                    MB_OK | MB_ICONINFORMATION,
                                );
                            }
                        }
                    }
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
}
