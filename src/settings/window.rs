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

pub const WM_APP_SETTINGS_SAVED: u32 = 0x8000 + 5;

static ACTIVE_SETTINGS_HWND: AtomicUsize = AtomicUsize::new(0);

const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

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

// Tab 1 (Windows)
const IDC_WIN_LIST: usize = 301;
const IDC_WIN_SCENE_COMBO: usize = 302;
const IDC_WIN_BIND: usize = 303;
const IDC_WIN_UNBIND: usize = 304;
const IDC_WIN_ALWAYS_VISIBLE: usize = 305;
const IDC_WIN_REFRESH: usize = 306;
const IDC_WIN_DETAILS: usize = 307;

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

#[cfg(windows)]
pub struct SettingsWindow {
    pub hwnd: windows::Win32::Foundation::HWND,
    pub parent_hwnd: windows::Win32::Foundation::HWND,
    pub vm: SettingsViewModel,
    pub selected_scene_idx: Option<usize>,
    pub selected_window_idx: Option<usize>,
    pub tab_controls: [Vec<windows::Win32::Foundation::HWND>; 4],
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
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::Controls::{
            InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
            INITCOMMONCONTROLSEX,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, LoadCursorW, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW,
            ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW,
            SW_SHOW, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
        };

        // If settings window is already open, activate and bring it to front
        let existing = ACTIVE_SETTINGS_HWND.load(Ordering::Acquire);
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            unsafe {
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

            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                class_name,
                w!("Kiri 設定"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                780,
                580,
                parent_hwnd,
                None,
                hinstance,
                None,
            )
            .unwrap();

            ACTIVE_SETTINGS_HWND.store(hwnd.0 as usize, Ordering::Release);

            let mut win = Box::new(Self {
                hwnd,
                parent_hwnd,
                vm,
                selected_scene_idx: None,
                selected_window_idx: None,
                tab_controls: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            });

            win.init_controls(hinstance);

            let ptr = &mut *win as *mut Self as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);

            // Box is intentionally leaked while the window is alive; dropped in WM_NCDESTROY
            let _ = Box::into_raw(win);

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    unsafe fn init_controls(&mut self, hinstance: windows::Win32::Foundation::HMODULE) {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT};
        use windows::Win32::UI::Controls::{TCIF_TEXT, TCITEMW, TCM_INSERTITEMW, WC_TABCONTROLW};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, SendMessageW, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST,
            ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY, LBS_NOTIFY, WS_BORDER, WS_CHILD, WS_VISIBLE,
            WS_VSCROLL,
        };

        let font = GetStockObject(DEFAULT_GUI_FONT);

        // Helper closure to create and font a control
        let create_ctrl = |class: PCWSTR,
                           text: PCWSTR,
                           style_u32: u32,
                           x: i32,
                           y: i32,
                           w: i32,
                           h: i32,
                           id: usize|
         -> HWND {
            let hctrl = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                class,
                text,
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    style_u32 | WS_CHILD.0 | WS_VISIBLE.0,
                ),
                x,
                y,
                w,
                h,
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _),
                hinstance,
                None,
            )
            .unwrap();
            let _ = SendMessageW(hctrl, 0x0030 /* WM_SETFONT */, WPARAM(font.0 as usize), LPARAM(1));
            hctrl
        };

        // Tab Control
        let tab_hwnd = create_ctrl(WC_TABCONTROLW, w!(""), 0, 10, 10, 745, 470, IDC_TAB_CONTROL);

        let tab_titles = [w!("シーン"), w!("ウィンドウ"), w!("ランチャー"), w!("動作")];
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
        let _ = create_ctrl(w!("BUTTON"), w!("OK"), BS_PUSHBUTTON as u32, 480, 495, 80, 30, IDC_BTN_OK);
        let _ = create_ctrl(
            w!("BUTTON"),
            w!("キャンセル"),
            BS_PUSHBUTTON as u32,
            570,
            495,
            80,
            30,
            IDC_BTN_CANCEL,
        );
        let _ = create_ctrl(
            w!("BUTTON"),
            w!("適用"),
            BS_PUSHBUTTON as u32,
            660,
            495,
            80,
            30,
            IDC_BTN_APPLY,
        );

        // ===================== Tab 0: Scenes =====================
        let mut t0 = Vec::new();
        t0.push(create_ctrl(w!("STATIC"), w!("登録済みシーン一覧:"), 0, 25, 45, 200, 20, 0));
        let scene_list = create_ctrl(
            w!("LISTBOX"),
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (LBS_NOTIFY as u32),
            25,
            70,
            240,
            340,
            IDC_SCENE_LIST,
        );
        t0.push(scene_list);

        t0.push(create_ctrl(w!("BUTTON"), w!("新規作成"), BS_PUSHBUTTON as u32, 25, 420, 80, 28, IDC_SCENE_ADD));
        t0.push(create_ctrl(w!("BUTTON"), w!("削除"), BS_PUSHBUTTON as u32, 110, 420, 70, 28, IDC_SCENE_DEL));
        t0.push(create_ctrl(w!("BUTTON"), w!("↑"), BS_PUSHBUTTON as u32, 185, 420, 38, 28, IDC_SCENE_UP));
        t0.push(create_ctrl(w!("BUTTON"), w!("↓"), BS_PUSHBUTTON as u32, 227, 420, 38, 28, IDC_SCENE_DOWN));

        // Right details area
        t0.push(create_ctrl(w!("STATIC"), w!("シーンID:"), 0, 290, 70, 80, 20, 0));
        t0.push(create_ctrl(w!("EDIT"), w!(""), WS_BORDER.0 | (ES_READONLY as u32), 290, 95, 240, 24, IDC_SCENE_ID));

        t0.push(create_ctrl(w!("STATIC"), w!("表示名:"), 0, 290, 130, 80, 20, 0));
        t0.push(create_ctrl(w!("EDIT"), w!(""), WS_BORDER.0 | (ES_AUTOHSCROLL as u32), 290, 155, 240, 24, IDC_SCENE_NAME));

        t0.push(create_ctrl(w!("STATIC"), w!("アイコン (chart / code / globe または .ico パス):"), 0, 290, 190, 400, 20, 0));
        t0.push(create_ctrl(w!("EDIT"), w!(""), WS_BORDER.0 | (ES_AUTOHSCROLL as u32), 290, 215, 380, 24, IDC_SCENE_ICON));

        t0.push(create_ctrl(
            w!("BUTTON"),
            w!("ランチャーに固定表示する (pinned)"),
            BS_AUTOCHECKBOX as u32,
            290,
            260,
            300,
            24,
            IDC_SCENE_PINNED,
        ));
        t0.push(create_ctrl(w!("BUTTON"), w!("変更を反映"), BS_PUSHBUTTON as u32, 290, 300, 110, 30, IDC_SCENE_UPDATE));
        self.tab_controls[0] = t0;

        // ===================== Tab 1: Windows =====================
        let mut t1 = Vec::new();
        t1.push(create_ctrl(w!("STATIC"), w!("開いているトップレベルウィンドウ一覧:"), 0, 25, 45, 300, 20, 0));
        let win_list = create_ctrl(
            w!("LISTBOX"),
            w!(""),
            WS_BORDER.0 | WS_VSCROLL.0 | (LBS_NOTIFY as u32),
            25,
            70,
            710,
            220,
            IDC_WIN_LIST,
        );
        t1.push(win_list);

        t1.push(create_ctrl(
            w!("EDIT"),
            w!(""),
            WS_BORDER.0 | (ES_MULTILINE as u32) | (ES_READONLY as u32) | WS_VSCROLL.0,
            25,
            300,
            710,
            90,
            IDC_WIN_DETAILS,
        ));

        t1.push(create_ctrl(w!("STATIC"), w!("登録先シーン:"), 0, 25, 405, 90, 20, 0));
        t1.push(create_ctrl(w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32, 115, 402, 160, 200, IDC_WIN_SCENE_COMBO));
        t1.push(create_ctrl(
            w!("BUTTON"),
            w!("常時表示 (always_visible)"),
            BS_AUTOCHECKBOX as u32,
            290,
            404,
            190,
            24,
            IDC_WIN_ALWAYS_VISIBLE,
        ));
        t1.push(create_ctrl(w!("BUTTON"), w!("シーンに登録"), BS_PUSHBUTTON as u32, 490, 400, 110, 28, IDC_WIN_BIND));
        t1.push(create_ctrl(w!("BUTTON"), w!("登録解除"), BS_PUSHBUTTON as u32, 610, 400, 90, 28, IDC_WIN_UNBIND));
        t1.push(create_ctrl(w!("BUTTON"), w!("一覧更新"), BS_PUSHBUTTON as u32, 25, 435, 90, 28, IDC_WIN_REFRESH));
        self.tab_controls[1] = t1;

        // ===================== Tab 2: Launcher =====================
        let mut t2 = Vec::new();
        t2.push(create_ctrl(w!("STATIC"), w!("配置方向:"), 0, 30, 60, 160, 20, 0));
        t2.push(create_ctrl(w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32, 210, 58, 160, 120, IDC_LCH_ORIENT_COMBO));

        t2.push(create_ctrl(w!("STATIC"), w!("ボタンサイズ (DIP):"), 0, 30, 100, 160, 20, 0));
        t2.push(create_ctrl(w!("EDIT"), w!("32"), WS_BORDER.0 | (ES_AUTOHSCROLL as u32), 210, 98, 80, 24, IDC_LCH_BTN_SIZE));

        t2.push(create_ctrl(w!("STATIC"), w!("グリップサイズ (DIP):"), 0, 30, 140, 160, 20, 0));
        t2.push(create_ctrl(w!("EDIT"), w!("18"), WS_BORDER.0 | (ES_AUTOHSCROLL as u32), 210, 138, 80, 24, IDC_LCH_GRIP_SIZE));

        t2.push(create_ctrl(w!("STATIC"), w!("常時表示件数 (visible_slots):"), 0, 30, 180, 180, 20, 0));
        t2.push(create_ctrl(w!("EDIT"), w!("3"), WS_BORDER.0 | (ES_AUTOHSCROLL as u32), 210, 178, 80, 24, IDC_LCH_SLOTS));

        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("常時最前面に表示する (always_on_top)"),
            BS_AUTOCHECKBOX as u32,
            30,
            225,
            320,
            24,
            IDC_LCH_TOPMOST,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("画面端にスナップする (snap_to_edges, 12 DIP以内)"),
            BS_AUTOCHECKBOX as u32,
            30,
            260,
            360,
            24,
            IDC_LCH_SNAP,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("シーン切替時に最前面ウィンドウへフォーカスを移動する (focus_on_switch)"),
            BS_AUTOCHECKBOX as u32,
            30,
            295,
            460,
            24,
            IDC_LCH_FOCUS,
        ));
        t2.push(create_ctrl(
            w!("BUTTON"),
            w!("ランチャーの位置を初期化"),
            BS_PUSHBUTTON as u32,
            30,
            345,
            180,
            30,
            IDC_LCH_RESET_POS,
        ));
        self.tab_controls[2] = t2;

        // ===================== Tab 3: Behavior =====================
        let mut t3 = Vec::new();
        t3.push(create_ctrl(w!("STATIC"), w!("制御対象モニター:"), 0, 30, 60, 160, 20, 0));
        t3.push(create_ctrl(w!("COMBOBOX"), w!(""), CBS_DROPDOWNLIST as u32, 200, 58, 380, 200, IDC_BEH_MONITOR_COMBO));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("Windows ログオン時に自動起動する (HKCU\\Run)"),
            BS_AUTOCHECKBOX as u32,
            30,
            110,
            380,
            24,
            IDC_BEH_AUTOSTART,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("ログにウィンドウタイトル全文を記録する (口座番号等のマスクを解除)"),
            BS_AUTOCHECKBOX as u32,
            30,
            145,
            480,
            24,
            IDC_BEH_VERBOSE_TITLE,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("Kiri が最小化した全ウィンドウを復元"),
            BS_PUSHBUTTON as u32,
            30,
            200,
            280,
            32,
            IDC_BEH_RESTORE_ALL,
        ));

        t3.push(create_ctrl(
            w!("BUTTON"),
            w!("設定保存先フォルダを開く"),
            BS_PUSHBUTTON as u32,
            30,
            245,
            180,
            30,
            IDC_BEH_OPEN_FOLDER,
        ));
        self.tab_controls[3] = t3;

        self.populate_data();
        self.switch_tab(0);
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
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageW, SetWindowTextW, BM_SETCHECK, CB_ADDSTRING, CB_RESETCONTENT, CB_SETCURSEL,
            LB_ADDSTRING, LB_RESETCONTENT, LB_SETCURSEL,
        };

        unsafe {
            // Tab 0: Scene list
            let h_list = dlg_item(self.hwnd, IDC_SCENE_LIST);
            let _ = SendMessageW(h_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
            for scene in &self.vm.config_draft.scenes {
                let pin_mark = if scene.pinned { "📌" } else { "  " };
                let text = format!("{} #{}: {} [{}]", pin_mark, scene.order, scene.name, scene.id);
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_list, LB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as isize));
            }
            if !self.vm.config_draft.scenes.is_empty() {
                let _ = SendMessageW(h_list, LB_SETCURSEL, WPARAM(0), LPARAM(0));
                self.selected_scene_idx = Some(0);
                self.load_selected_scene_details(0);
            }

            // Tab 1: Windows list & Scene combo
            let h_win_list = dlg_item(self.hwnd, IDC_WIN_LIST);
            let _ = SendMessageW(h_win_list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
            for row in &self.vm.candidate_windows {
                let warn_tag = if !row.warnings.is_empty() { " ⚠️警告あり" } else { "" };
                let bound_str = if row.bound_scene_ids.is_empty() {
                    "未登録".to_string()
                } else {
                    row.bound_scene_ids.join(",")
                };
                let text = format!(
                    "[{}] [シーン: {}] {} ({}){}",
                    row.instance.pid, bound_str, row.instance.title, row.instance.exe_path, warn_tag
                );
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_win_list, LB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as isize));
            }
            if !self.vm.candidate_windows.is_empty() {
                let _ = SendMessageW(h_win_list, LB_SETCURSEL, WPARAM(0), LPARAM(0));
                self.selected_window_idx = Some(0);
                self.load_selected_window_details(0);
            }

            let h_combo = dlg_item(self.hwnd, IDC_WIN_SCENE_COMBO);
            let _ = SendMessageW(h_combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
            for scene in &self.vm.config_draft.scenes {
                let wide: Vec<u16> = scene.name.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_combo, CB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as isize));
            }
            let _ = SendMessageW(h_combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));

            // Tab 2: Launcher options
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

            // Tab 3: Behavior
            let h_mon_combo = dlg_item(self.hwnd, IDC_BEH_MONITOR_COMBO);
            let _ = SendMessageW(h_mon_combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
            let mut cur_mon_sel = 0;
            for (idx, mon) in self.vm.monitors.iter().enumerate() {
                let prim = if mon.is_primary { " (プライマリ)" } else { "" };
                let txt = format!("{}: {}{}", mon.monitor_id, mon.friendly_name, prim);
                let w_txt: Vec<u16> = txt.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SendMessageW(h_mon_combo, CB_ADDSTRING, WPARAM(0), LPARAM(w_txt.as_ptr() as isize));
                if self.vm.config_draft.target.monitor_id.as_deref() == Some(&mon.monitor_id) {
                    cur_mon_sel = idx;
                }
            }
            let s_follow: Vec<u16> = "ランチャー所在モニターに自動追従".encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SendMessageW(h_mon_combo, CB_ADDSTRING, WPARAM(0), LPARAM(s_follow.as_ptr() as isize));
            if self.vm.config_draft.target.monitor_policy == MonitorPolicy::LauncherMonitor {
                cur_mon_sel = self.vm.monitors.len();
            }
            let _ = SendMessageW(h_mon_combo, CB_SETCURSEL, WPARAM(cur_mon_sel), LPARAM(0));

            set_chk(IDC_BEH_AUTOSTART, self.vm.autostart_enabled);
            set_chk(IDC_BEH_VERBOSE_TITLE, crate::diagnostics::is_verbose_title_logging());
        }
    }

    pub fn load_selected_scene_details(&mut self, idx: usize) {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, SetWindowTextW, BM_SETCHECK};

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
            }
        }
    }

    pub fn load_selected_window_details(&mut self, idx: usize) {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, SetWindowTextW, BM_SETCHECK};

        if let Some(row) = self.vm.candidate_windows.get(idx) {
            let mut details = format!(
                "PID: {}\r\nEXEパス: {}\r\nクラス名: {}\r\n所在モニター: {}\r\n状態: {} / {}\r\n",
                row.instance.pid,
                row.instance.exe_path,
                row.instance.window_class,
                row.instance.monitor_id,
                if row.instance.is_visible { "可視" } else { "非表示" },
                if row.instance.is_iconic { "最小化中" } else { "通常表示" }
            );

            if !row.warnings.is_empty() {
                details.push_str("\r\n【登録に関する警告】\r\n");
                for w in &row.warnings {
                    match w {
                        crate::scenes::bindings::RegistrationWarning::NoTaskbarButton => {
                            details.push_str("・⚠️ タスクバーボタンがありません (最小化後の手動復旧経路が制限されます)\r\n");
                        }
                        crate::scenes::bindings::RegistrationWarning::ElevatedProcess => {
                            details.push_str("・⚠️ 管理者権限プロセスです (UIPI制限により操作できない場合があります)\r\n");
                        }
                        crate::scenes::bindings::RegistrationWarning::OutsideTargetMonitor { window_monitor } => {
                            details.push_str(&format!("・⚠️ 制御対象外モニター ({}) に配置されています\r\n", window_monitor));
                        }
                        crate::scenes::bindings::RegistrationWarning::TitleMayContainSensitiveOrDynamicText => {
                            details.push_str("・⚠️ タイトルに動的情報（口座番号・銘柄等）が含まれている可能性があります\r\n");
                        }
                    }
                }
            }

            unsafe {
                let h_det = dlg_item(self.hwnd, IDC_WIN_DETAILS);
                let w: Vec<u16> = details.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(h_det, PCWSTR(w.as_ptr()));

                let h_av = dlg_item(self.hwnd, IDC_WIN_ALWAYS_VISIBLE);
                let st = if row.always_visible { BST_CHECKED } else { BST_UNCHECKED };
                let _ = SendMessageW(h_av, BM_SETCHECK, WPARAM(st), LPARAM(0));
            }
        }
    }

    pub fn save_changes_to_config(&mut self) -> Result<(), String> {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowTextW, SendMessageW, BM_GETCHECK, CB_GETCURSEL,
        };

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

            // Read Behavior controls
            let h_mon = dlg_item(self.hwnd, IDC_BEH_MONITOR_COMBO);
            let mon_sel = SendMessageW(h_mon, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
            if mon_sel < self.vm.monitors.len() {
                self.vm.config_draft.target.monitor_policy = MonitorPolicy::Fixed;
                self.vm.config_draft.target.monitor_id = Some(self.vm.monitors[mon_sel].monitor_id.clone());
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
        DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowTextW, MessageBoxW,
        PostMessageW, SendMessageW, SetWindowLongPtrW, BM_GETCHECK, CB_GETCURSEL,
        GWLP_USERDATA, LBN_SELCHANGE, LB_GETCURSEL, MB_ICONINFORMATION, MB_OK,
        WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_NCDESTROY, WM_NOTIFY,
    };

    let result = std::panic::catch_unwind(|| {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsWindow;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let win = &mut *ptr;

        match msg {
            WM_NOTIFY => {
                let nmhdr = &*(lparam.0 as *const NMHDR);
                if nmhdr.idFrom == IDC_TAB_CONTROL
                    && nmhdr.code == windows::Win32::UI::Controls::TCN_SELCHANGE
                {
                    let cur_tab = SendMessageW(
                        nmhdr.hwndFrom,
                        windows::Win32::UI::Controls::TCM_GETCURSEL,
                        WPARAM(0),
                        LPARAM(0),
                    )
                    .0 as usize;
                    win.switch_tab(cur_tab);
                    return LRESULT(0);
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
                        let h = dlg_item(hwnd, IDC_SCENE_LIST);
                        let sel = SendMessageW(h, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                        win.selected_scene_idx = Some(sel);
                        win.load_selected_scene_details(sel);
                        LRESULT(0)
                    }
                    IDC_SCENE_ADD => {
                        let new_id = format!("scene-{}", win.vm.config_draft.scenes.len() + 1);
                        let new_order = win.vm.config_draft.scenes.len() as i32 + 1;
                        win.vm.config_draft.scenes.push(Scene {
                            id: new_id.clone(),
                            name: format!("新規シーン {}", new_order),
                            icon: "chart".to_string(),
                            order: new_order,
                            pinned: true,
                        });
                        win.populate_data();
                        LRESULT(0)
                    }
                    IDC_SCENE_DEL => {
                        if let Some(idx) = win.selected_scene_idx {
                            if win.vm.config_draft.scenes.len() > 1 && idx < win.vm.config_draft.scenes.len() {
                                win.vm.config_draft.scenes.remove(idx);
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_UP => {
                        if let Some(idx) = win.selected_scene_idx {
                            if idx > 0 && idx < win.vm.config_draft.scenes.len() {
                                win.vm.config_draft.scenes.swap(idx, idx - 1);
                                for (i, s) in win.vm.config_draft.scenes.iter_mut().enumerate() {
                                    s.order = i as i32 + 1;
                                }
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_DOWN => {
                        if let Some(idx) = win.selected_scene_idx {
                            if idx + 1 < win.vm.config_draft.scenes.len() {
                                win.vm.config_draft.scenes.swap(idx, idx + 1);
                                for (i, s) in win.vm.config_draft.scenes.iter_mut().enumerate() {
                                    s.order = i as i32 + 1;
                                }
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_SCENE_UPDATE => {
                        if let Some(idx) = win.selected_scene_idx {
                            if let Some(scene) = win.vm.config_draft.scenes.get_mut(idx) {
                                let get_text = |ctrl_id: usize| -> String {
                                    let h = dlg_item(hwnd, ctrl_id);
                                    let mut buf = [0u16; 256];
                                    let len = GetWindowTextW(h, &mut buf);
                                    if len > 0 {
                                        String::from_utf16_lossy(&buf[..len as usize])
                                    } else {
                                        String::new()
                                    }
                                };
                                let name = get_text(IDC_SCENE_NAME);
                                let icon = get_text(IDC_SCENE_ICON);
                                let h_chk = dlg_item(hwnd, IDC_SCENE_PINNED);
                                let pinned = SendMessageW(h_chk, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED as isize;

                                if !name.is_empty() {
                                    scene.name = name;
                                }
                                if !icon.is_empty() {
                                    scene.icon = icon;
                                }
                                scene.pinned = pinned;
                                win.populate_data();
                            }
                        }
                        LRESULT(0)
                    }

                    // Tab 1 Window selection changed
                    IDC_WIN_LIST if code == LBN_SELCHANGE => {
                        let h = dlg_item(hwnd, IDC_WIN_LIST);
                        let sel = SendMessageW(h, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                        win.selected_window_idx = Some(sel);
                        win.load_selected_window_details(sel);
                        LRESULT(0)
                    }
                    IDC_WIN_BIND => {
                        if let Some(idx) = win.selected_window_idx {
                            let h_combo = dlg_item(hwnd, IDC_WIN_SCENE_COMBO);
                            let scene_sel = SendMessageW(h_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                            let h_av = dlg_item(hwnd, IDC_WIN_ALWAYS_VISIBLE);
                            let always_visible =
                                SendMessageW(h_av, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                                    == BST_CHECKED as isize;
                            if let Some(scene) = win.vm.config_draft.scenes.get(scene_sel) {
                                let scene_id = scene.id.clone();
                                if let Some(row) = win.vm.candidate_windows.get(idx) {
                                    let win_instance = row.instance.clone();
                                    win.vm.bind_window_to_scene(
                                        &win_instance,
                                        &scene_id,
                                        always_visible,
                                    );
                                    win.populate_data();
                                }
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_WIN_UNBIND => {
                        if let Some(idx) = win.selected_window_idx {
                            if let Some(row) = win.vm.candidate_windows.get(idx) {
                                if let Some(ref bid) = row.existing_binding_id {
                                    let bid_owned = bid.clone();
                                    win.vm.config_draft.bindings.retain(|b| b.id != bid_owned);
                                    win.vm.sync_binding_annotations();
                                    win.populate_data();
                                }
                            }
                        }
                        LRESULT(0)
                    }
                    IDC_WIN_REFRESH => {
                        let sys = crate::platform::windows::Win32WindowSystem::new();
                        win.vm.refresh_candidates(&sys);
                        win.populate_data();
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
