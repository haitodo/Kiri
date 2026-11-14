//! Fixed-size native host for the on-demand, local settings page.

use super::{
    editor::{app_label, candidate_key, Command, Editor, FileKind, SettingsPage},
    files::{open_file_dialog, save_file_dialog},
};
use crate::app::AppController;
use crate::diagnostics::{log_error, log_info, log_warn};
use crate::platform::monitors::dip_to_px;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::OnceLock,
};
use webview2_com::{
    CoTaskMemPWSTR, CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, ProcessFailedEventHandler,
    WebMessageReceivedEventHandler, WebResourceRequestedEventHandler,
};
use webview_windows::{
    core::{Interface as _, PCWSTR as WebStr, PWSTR as WebMutStr},
    Win32::{
        Foundation::{HWND as WebHwnd, RECT as WebRect},
        UI::Shell::SHCreateMemStream,
    },
};
use windows::{
    core::{w, HSTRING},
    Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Win32::Graphics::{
        Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE},
        Gdi::*,
    },
    Win32::System::{
        Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_CENTER, SS_NOPREFIX},
    },
    Win32::UI::{
        HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

pub(crate) const WM_SETTINGS_REQUEST: u32 = WM_APP + 5;
const WM_SETTINGS_READY: u32 = WM_APP + 10;
const WM_SETTINGS_FAILED: u32 = WM_APP + 11;
const WM_SETTINGS_CLOSE: u32 = WM_APP + 12;
const TIMER_STARTUP: usize = 1;
const ID_RETRY: usize = 1;
const SETTINGS_STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const PAGE: &str = concat!(
    include_str!("ui/head.html"),
    "<style>",
    include_str!("ui/settings.css"),
    "</style></head><body>",
    include_str!("ui/body.html"),
    "<script>",
    include_str!("ui/settings.js"),
    "</script></body></html>",
);
const LOCAL_DOCUMENT: &str = "https://settings.kiri.invalid/";

thread_local! {
    // The message carries only the sender HWND; only our WebView callbacks enqueue requests.
    static REQUESTS: RefCell<std::collections::VecDeque<(HWND, Request)>> = const { RefCell::new(std::collections::VecDeque::new()) };
    static ACTIVE: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static NEXT_ATTEMPT: Cell<u32> = const { Cell::new(0) };
}

#[derive(Deserialize)]
struct Request {
    id: u32,
    #[serde(flatten)]
    command: Command,
}

enum FileAction {
    Import,
    Export,
    OpenFolder,
    Browse(FileKind),
}

struct FileRequest {
    hwnd: HWND,
    id: u32,
    action: FileAction,
}

thread_local! {
    static FILE_REQUEST: RefCell<Option<FileRequest>> = const { RefCell::new(None) };
}

/// Called by the main loop after dispatching the launcher message. Shell dialogs run their
/// own modal loop, so no mutable borrow of the launcher may be held here.
pub(crate) fn run_file_request() {
    let Some(FileRequest { hwnd, id, action }) = FILE_REQUEST.with_borrow_mut(Option::take) else {
        return;
    };
    unsafe {
        let settings_path = crate::config::ConfigManager::default_config_path();
        let json_files = [("設定ファイル (*.json)", "*.json")];
        let path = match action {
            FileAction::Import => open_file_dialog(hwnd, false, &json_files).map_err(file_error),
            FileAction::Export => save_file_dialog(hwnd, "kiri-settings.json", "json", &json_files)
                .map_err(file_error),
            FileAction::Browse(kind) => {
                let images = [("画像ファイル (*.ico;*.png;*.bmp)", "*.ico;*.png;*.bmp")];
                open_file_dialog(
                    hwnd,
                    matches!(kind, FileKind::Folder),
                    if matches!(kind, FileKind::Image) {
                        &images
                    } else {
                        &[]
                    },
                )
                .map_err(file_error)
            }
            FileAction::OpenFolder => {
                let folder = HSTRING::from(
                    settings_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .as_os_str(),
                );
                let result = ShellExecuteW(hwnd, w!("open"), &folder, None, None, SW_SHOWNORMAL);
                if result.0 as isize <= 32 {
                    Err("設定の保存先を開けませんでした。".into())
                } else {
                    Ok(None)
                }
            }
        };
        let Some(window) = SettingsWindow::get(hwnd) else {
            return;
        };
        window.file_pending.set(false);
        let command = match (action, path) {
            (_, Err(error)) => {
                window.post(json!({ "kind": "reply", "id": id, "error": error, "ok": false }));
                return;
            }
            (FileAction::Import, Ok(Some(path))) => Command::ImportPath(path),
            (FileAction::Export, Ok(Some(path))) => Command::ExportPath(path),
            (FileAction::Browse(_), Ok(path)) => {
                window.post(
                    json!({ "kind": "reply", "id": id, "extra": { "path": path }, "ok": true }),
                );
                return;
            }
            (_, Ok(None)) => {
                window.post(json!({ "kind": "reply", "id": id, "ok": true }));
                return;
            }
            (FileAction::OpenFolder, Ok(Some(_))) => unreachable!(),
        };
        enqueue(hwnd, Request { id, command });
    }
}

unsafe fn enqueue(hwnd: HWND, request: Request) {
    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        REQUESTS.with_borrow_mut(|requests| {
            requests.push_back((hwnd, request));
            if PostMessageW(
                owner,
                WM_SETTINGS_REQUEST,
                WPARAM(hwnd.0 as usize),
                LPARAM(0),
            )
            .is_err()
            {
                requests.pop_back();
            }
        });
    }
}

pub struct SettingsWindow {
    hwnd: HWND,
    loading_label: HWND,
    retry: HWND,
    icon: HICON,
    editor: RefCell<Editor>,
    page: Cell<SettingsPage>,
    controller: RefCell<Option<ICoreWebView2Controller>>,
    initializing: Cell<bool>,
    attempt: Cell<u32>,
    file_pending: Cell<bool>,
    _apartment: Apartment,
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

impl SettingsWindow {
    pub(crate) unsafe fn show_hotkey_conflicts(hwnd: HWND, names: &[String]) {
        if !names.is_empty() {
            if let Some(window) = Self::get(hwnd) {
                window.post(json!({ "kind": "warning", "message": format!(
                    "設定は保存しましたが、次のシーンのキーを Windows に登録できませんでした：{}", names.join("、")
                ) }));
            }
        }
    }

    pub fn show(owner: HWND, app: &AppController, page: SettingsPage) -> Result<(), String> {
        unsafe {
            let existing = ACTIVE.with(Cell::get);
            if !existing.0.is_null() {
                let hwnd = existing;
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
                if let Some(window) = Self::get(hwnd) {
                    window.page.set(page);
                    window.post(json!({ "kind": "page", "page": page.id() }));
                }
                return Ok(());
            }

            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|e| e.to_string())?;
            let apartment = Apartment;
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = w!("KiriSettingsWindowClass");
            static REGISTERED: OnceLock<bool> = OnceLock::new();
            if !*REGISTERED.get_or_init(|| {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance.into(),
                    hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class,
                    ..Default::default()
                }) != 0
            }) {
                return Err("設定ウィンドウを登録できませんでした。".into());
            }
            let last_monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
            let area = work_area(last_monitor).unwrap_or(RECT {
                left: 0,
                top: 0,
                right: 1024,
                bottom: 768,
            });
            let (width, height) = window_size(GetDpiForWindow(owner).max(96), area);
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                class,
                w!("Kiri 設定"),
                SETTINGS_STYLE,
                area.left + (area.right - area.left - width) / 2,
                area.top + (area.bottom - area.top - height) / 2,
                width,
                height,
                owner,
                None,
                instance,
                None,
            )
            .map_err(|e| e.to_string())?;
            let loading_label = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("設定を読み込んでいます…"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_CENTER.0 | SS_NOPREFIX.0),
                0,
                0,
                0,
                0,
                hwnd,
                None,
                instance,
                None,
            )
            .unwrap_or_default();
            let retry = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("再試行"),
                WS_CHILD | WS_TABSTOP,
                0,
                0,
                0,
                0,
                hwnd,
                HMENU(ID_RETRY as *mut _),
                instance,
                None,
            )
            .unwrap_or_default();
            for child in [loading_label, retry] {
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize),
                    LPARAM(1),
                );
            }
            let icon = crate::launcher::paint::create_kiri_hicon(32).unwrap_or_default();
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(icon.0 as isize),
            );
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(icon.0 as isize),
            );
            let rounding = 2u32; // DWMWCP_ROUND
            let _ = DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWA_WINDOW_CORNER_PREFERENCE,
                (&rounding as *const u32).cast(),
                4,
            );
            let window = Box::new(Self {
                hwnd,
                loading_label,
                retry,
                icon,
                editor: RefCell::new(Editor::new(app)),
                page: Cell::new(page),
                controller: RefCell::new(None),
                initializing: Cell::new(false),
                attempt: Cell::new(0),
                file_pending: Cell::new(false),
                _apartment: apartment,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(window) as isize);
            ACTIVE.with(|active| active.set(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            if PostMessageW(hwnd, WM_SETTINGS_READY, WPARAM(0), LPARAM(0)).is_err() {
                let _ = DestroyWindow(hwnd);
                return Err("設定画面を読み込めませんでした。".into());
            }
        }
        Ok(())
    }

    unsafe fn get<'a>(hwnd: HWND) -> Option<&'a Self> {
        (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Self).as_ref()
    }

    unsafe fn begin_webview(&self) {
        if self.initializing.get() || self.controller.borrow().is_some() {
            return;
        }
        self.initializing.set(true);
        let attempt = NEXT_ATTEMPT.with(|counter| {
            let next = counter.get().wrapping_add(1);
            counter.set(next);
            next
        });
        self.attempt.set(attempt);
        let hwnd = self.hwnd;
        let _ = SetWindowTextW(self.loading_label, w!("設定を読み込んでいます…"));
        let _ = ShowWindow(self.retry, SW_HIDE);
        let _ = SetTimer(hwnd, TIMER_STARTUP, 15_000, None);
        let profile = CoTaskMemPWSTR::from(
            crate::diagnostics::default_app_data_dir()
                .join("SettingsWebView")
                .to_string_lossy()
                .as_ref(),
        );
        let options: ICoreWebView2EnvironmentOptions =
            CoreWebView2EnvironmentOptions::default().into();
        // The page is entirely bundled; it never needs browser background services.
        let flags = CoTaskMemPWSTR::from(
            "--disable-background-networking --disable-sync --disable-component-update",
        );
        let _ = options.SetAdditionalBrowserArguments(*flags.as_ref().as_pcwstr());
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |result, environment| {
                if !Self::get(hwnd)
                    .is_some_and(|w| w.initializing.get() && w.attempt.get() == attempt)
                {
                    return Ok(());
                }
                guarded(hwnd, || {
                    result?;
                    log_info("settings", "Settings browser environment initialized");
                    if let Some((window, environment)) = Self::get(hwnd).zip(environment) {
                        if !window.initializing.get() {
                            return Ok(());
                        }
                        let env = environment.cast::<ICoreWebView2Environment10>()?;
                        let opts = env.CreateCoreWebView2ControllerOptions()?;
                        opts.SetIsInPrivateModeEnabled(true)?;
                        env.CreateCoreWebView2ControllerWithOptions(
                            WebHwnd(hwnd.0),
                            &opts,
                            &CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                                move |result, controller| {
                                    if !Self::get(hwnd).is_some_and(|w| {
                                        w.initializing.get() && w.attempt.get() == attempt
                                    }) {
                                        if let Some(controller) = controller {
                                            let _ = controller.Close();
                                        }
                                        return Ok(());
                                    }
                                    guarded(hwnd, || {
                                        result?;
                                        if let Some((window, controller)) =
                                            Self::get(hwnd).zip(controller)
                                        {
                                            log_info(
                                                "settings",
                                                "Settings browser controller initialized",
                                            );
                                            window
                                                .attach(controller.clone(), &environment)
                                                .inspect_err(|_| {
                                                    if window.controller.borrow().is_none() {
                                                        let _ = controller.Close();
                                                    }
                                                })?;
                                        }
                                        Ok(())
                                    });
                                    Ok(())
                                },
                            )),
                        )?;
                    }
                    Ok(())
                });
                Ok(())
            },
        ));
        if let Err(error) = CreateCoreWebView2EnvironmentWithOptions(
            WebStr::null(),
            *profile.as_ref().as_pcwstr(),
            &options,
            &handler,
        ) {
            log_warn(
                "settings",
                &format!("WebView2 initialization failed: {error}"),
            );
            self.failed();
        }
    }

    unsafe fn attach(
        &self,
        controller: ICoreWebView2Controller,
        environment: &ICoreWebView2Environment,
    ) -> webview_windows::core::Result<()> {
        let view = controller.CoreWebView2()?;
        let settings = view.Settings()?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        settings.SetIsBuiltInErrorPageEnabled(false)?;
        settings
            .cast::<ICoreWebView2Settings3>()?
            .SetAreBrowserAcceleratorKeysEnabled(false)?;
        controller
            .cast::<ICoreWebView2Controller4>()?
            .SetAllowExternalDrop(false)?;

        let mut token = 0;
        // Serve the bundled page directly from memory. No loopback server or network requests.
        let environment = environment.clone();
        view.AddWebResourceRequestedFilter(
            webview_windows::core::w!("*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
        view.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut uri = WebMutStr::null();
                args.Request()?.Uri(&mut uri)?;
                let response = if CoTaskMemPWSTR::from(uri).to_string() == LOCAL_DOCUMENT {
                    let stream = SHCreateMemStream(Some(PAGE.as_bytes()))
                        .ok_or_else(webview_windows::core::Error::from_win32)?;
                    environment.CreateWebResourceResponse(
                        &stream,
                        200,
                        webview_windows::core::w!("OK"),
                        webview_windows::core::w!(
                            "Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store"
                        ),
                    )?
                } else {
                    environment.CreateWebResourceResponse(
                        None,
                        404,
                        webview_windows::core::w!("Not found"),
                        WebStr::null(),
                    )?
                };
                args.SetResponse(&response)
            })),
            &mut token,
        )?;
        view.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    let mut uri = WebMutStr::null();
                    args.Uri(&mut uri)?;
                    if CoTaskMemPWSTR::from(uri).to_string() != LOCAL_DOCUMENT {
                        args.SetCancel(true)?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        view.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        let hwnd = self.hwnd;
        let self_attempt = self.attempt.get();
        view.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    args.ProcessFailedKind(&mut kind)?;
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                    {
                        let _ = PostMessageW(
                            hwnd,
                            WM_SETTINGS_FAILED,
                            WPARAM(self_attempt as usize),
                            LPARAM(0),
                        );
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        view.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                guarded(hwnd, || {
                    if !Self::get(hwnd).is_some_and(|w| w.attempt.get() == self_attempt) {
                        return Ok(());
                    }
                    let Some(args) = args else { return Ok(()) };
                    let mut source = WebMutStr::null();
                    args.Source(&mut source)?;
                    let source = CoTaskMemPWSTR::from(source).to_string();
                    if source != LOCAL_DOCUMENT {
                        log_warn(
                            "settings",
                            "Ignored a message from outside the settings document",
                        );
                        return Ok(());
                    }
                    let mut message = WebMutStr::null();
                    args.WebMessageAsJson(&mut message)?;
                    let message = CoTaskMemPWSTR::from(message).to_string();
                    if message.len() > 256 * 1024 {
                        return Ok(());
                    }
                    if let Ok(request) = serde_json::from_str::<Request>(&message) {
                        enqueue(hwnd, request);
                    } else {
                        log_warn("settings", "Ignored an invalid settings request");
                    }
                    Ok(())
                });
                Ok(())
            })),
            &mut token,
        )?;

        *self.controller.borrow_mut() = Some(controller);
        let url = CoTaskMemPWSTR::from(LOCAL_DOCUMENT);
        view.Navigate(*url.as_ref().as_pcwstr())?;
        self.layout();
        Ok(())
    }

    unsafe fn layout(&self) {
        let mut area = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut area);
        if let Some(controller) = &*self.controller.borrow() {
            let _ = controller.SetBounds(WebRect {
                left: 0,
                top: 0,
                right: area.right,
                bottom: area.bottom,
            });
        }
        let dpi = GetDpiForWindow(self.hwnd).max(96);
        let pad = dip_to_px(24, dpi);
        let retry_width = dip_to_px(120, dpi);
        let center = area.bottom / 2;
        let _ = MoveWindow(
            self.loading_label,
            pad,
            center - dip_to_px(48, dpi),
            (area.right - 2 * pad).max(0),
            dip_to_px(75, dpi),
            true,
        );
        let _ = MoveWindow(
            self.retry,
            ((area.right - retry_width) / 2).max(0),
            center + dip_to_px(35, dpi),
            retry_width,
            dip_to_px(34, dpi),
            true,
        );
    }

    unsafe fn failed(&self) {
        let _ = KillTimer(self.hwnd, TIMER_STARTUP);
        self.initializing.set(false);
        if let Some(controller) = self.controller.borrow_mut().take() {
            let _ = controller.Close();
        }
        let _ = SetWindowTextW(self.loading_label, w!("設定画面を読み込めませんでした。\nMicrosoft Edge WebView2 ランタイムが利用できることを確認し、再試行してください。"));
        let _ = ShowWindow(self.loading_label, SW_SHOW);
        let _ = ShowWindow(self.retry, SW_SHOW);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(self.retry);
        self.layout();
    }

    fn post(&self, message: Value) {
        if let Some(controller) = &*self.controller.borrow() {
            let message = CoTaskMemPWSTR::from(message.to_string().as_str());
            unsafe {
                if let Ok(view) = controller.CoreWebView2() {
                    let _ = view.PostWebMessageAsJson(*message.as_ref().as_pcwstr());
                }
            }
        }
    }

    /// Handles one request and reports whether the running launcher should reload its saved options.
    pub(crate) unsafe fn dispatch(hwnd: HWND, owner: HWND, app: &mut AppController) -> bool {
        let Some(request) = REQUESTS.with_borrow_mut(|requests| {
            let index = requests.iter().position(|(sender, _)| *sender == hwnd)?;
            requests.remove(index).map(|(_, request)| request)
        }) else {
            return false;
        };
        if !GetWindow(hwnd, GW_OWNER).is_ok_and(|window| window == owner) {
            return false;
        }
        let Some(window) = Self::get(hwnd) else {
            return false;
        };
        let Request { id, command } = request;
        let file_action = match command {
            Command::Import if !window.editor.borrow().read_only => Some(FileAction::Import),
            Command::Export => Some(FileAction::Export),
            Command::OpenFolder => Some(FileAction::OpenFolder),
            Command::Browse { kind } => Some(FileAction::Browse(kind)),
            _ => None,
        };
        if let Some(action) = file_action {
            if !window.file_pending.replace(true) {
                FILE_REQUEST
                    .with_borrow_mut(|pending| *pending = Some(FileRequest { hwnd, id, action }));
            } else {
                window.post(json!({ "kind": "reply", "id": id, "error": "前のファイル選択が終わるまでお待ちください。", "ok": false }));
            }
            return false;
        }
        if let Command::Appearance { dark } = command {
            let dark = i32::from(dark);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const i32).cast(),
                4,
            );
            return false;
        }
        let mut changed = false;
        let result = window.request(command, app, &mut changed);
        let reply = match result {
            Ok(extra) => {
                json!({ "kind": "reply", "id": id, "extra": extra, "ok": true, "state": window.editor.borrow().state() })
            }
            Err(error) => json!({ "kind": "reply", "id": id, "error": error, "ok": false }),
        };
        window.post(reply);
        changed
    }

    unsafe fn request(
        &self,
        command: Command,
        app: &mut AppController,
        changed: &mut bool,
    ) -> Result<Value, String> {
        use Command::*;
        let mut editor = self.editor.borrow_mut();
        match command {
            Ready => {
                let _ = KillTimer(self.hwnd, TIMER_STARTUP);
                self.initializing.set(false);
                log_info("settings", "Settings page ready");
                let _ = ShowWindow(self.loading_label, SW_HIDE);
                if let Some(controller) = &*self.controller.borrow() {
                    let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
                Ok(json!({
                    "page": self.page.get().id(),
                    "first_run": !app.config.bindings.iter().any(|b| b.enabled),
                }))
            }
            Refresh => {
                editor.refresh(app);
                Ok(Value::Null)
            }
            Discard => {
                editor.reset(app);
                Ok(Value::Null)
            }
            Inspect { scene } => Ok(editor.inspect(&scene, app)),
            Save {
                tray_only_confirmed,
            } => {
                let (config_changed, warning) = editor.save(app, tray_only_confirmed)?;
                *changed = config_changed;
                Ok(json!({ "warning": warning }))
            }
            ImportPath(path) => {
                if editor.read_only {
                    return Err("この設定は読み取り専用です。".into());
                }
                let (config, warnings) = app
                    .config_mgr
                    .import_config_from_path(Path::new(&path))
                    .map_err(|e| format!("設定を読み込めませんでした。{e}"))?;
                editor.import(config, app);
                Ok(json!({ "imported": true, "warnings": warnings }))
            }
            ExportPath(path) => {
                let path = Path::new(&path);
                let config_file = app.config_mgr.config_path();
                if same_path(path, config_file) || same_path(path, &app.config_mgr.backup_path()) {
                    return Err("元の設定や自動バックアップと異なる場所を選んでください。".into());
                }
                app.config_mgr
                    .export_config_to_path(&app.config, path)
                    .map_err(|e| format!("設定を書き出せませんでした。{e}"))?;
                Ok(json!({ "exported": true }))
            }
            RecoveryPreview => {
                app.process_queued_win_events();
                Ok(json!(app
                    .recovery
                    .restorable_windows(&*app.sys)
                    .iter()
                    .map(|win| json!({
                        "app": app_label(win), "title": win.title, "key": candidate_key(win),
                    }))
                    .collect::<Vec<_>>()))
            }
            Restore { candidates } => {
                app.process_queued_win_events();
                let current = app.recovery.restorable_windows(&*app.sys);
                if current
                    .iter()
                    .any(|win| !candidates.contains(&candidate_key(win)))
                {
                    return Err(
                        "復元できるウィンドウが変わりました。対象を確認し直してください。".into(),
                    );
                }
                let count = app.restore_settings_windows(&current);
                editor.refresh(app);
                Ok(json!({ "restored": count }))
            }
            Close { discard } => {
                if editor.dirty() && !discard {
                    return Err("保存していない変更があります。".into());
                }
                let _ = PostMessageW(self.hwnd, WM_SETTINGS_CLOSE, WPARAM(0), LPARAM(0));
                Ok(Value::Null)
            }
            edit => editor.edit(edit, app),
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    crate::scenes::bindings::exe_paths_equal(&a.to_string_lossy(), &b.to_string_lossy())
}

fn file_error(error: windows::core::Error) -> String {
    format!("ファイル選択を開けませんでした。{error}")
}

fn guarded(hwnd: HWND, f: impl FnOnce() -> webview_windows::core::Result<()>) {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => return,
        Ok(Err(error)) => log_warn("settings", &format!("WebView2 callback failed: {error}")),
        Err(_) => log_error("settings", "Caught panic in settings WebView2 callback"),
    }
    unsafe {
        let _ = PostMessageW(hwnd, WM_SETTINGS_FAILED, WPARAM(0), LPARAM(0));
    }
}

unsafe fn work_area(monitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    GetMonitorInfoW(monitor, &mut info)
        .as_bool()
        .then_some(info.rcWork)
}

unsafe fn window_size(dpi: u32, area: RECT) -> (i32, i32) {
    let mut outer = RECT {
        right: dip_to_px(960, dpi),
        bottom: dip_to_px(620, dpi),
        ..Default::default()
    };
    let _ = AdjustWindowRectExForDpi(&mut outer, SETTINGS_STYLE, false, WS_EX_APPWINDOW, dpi);
    (
        (outer.right - outer.left).min(area.right - area.left),
        (outer.bottom - outer.top).min(area.bottom - area.top),
    )
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(window) = SettingsWindow::get(hwnd) else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_SETTINGS_READY => window.begin_webview(),
            WM_SETTINGS_FAILED if window.attempt.get() == wp.0 as u32 || wp.0 == 0 => {
                window.failed()
            }
            WM_TIMER if wp.0 == TIMER_STARTUP => {
                log_warn("settings", "Timed out waiting for the settings page");
                window.failed();
            }
            WM_COMMAND if wp.0 & 0xffff == ID_RETRY => window.begin_webview(),
            WM_SIZE => window.layout(),
            WM_MOVE => {
                if let Some(controller) = &*window.controller.borrow() {
                    let _ = controller.NotifyParentWindowPositionChanged();
                }
            }
            WM_DPICHANGED => {
                let dpi = ((wp.0 & 0xffff) as u32).max(96);
                let suggested = &*(lp.0 as *const RECT);
                let monitor = MonitorFromRect(suggested, MONITOR_DEFAULTTONEAREST);
                if let Some(area) = work_area(monitor) {
                    let (w, h) = window_size(dpi, area);
                    let x = suggested.left.clamp(area.left, area.right - w);
                    let y = suggested.top.clamp(area.top, area.bottom - h);
                    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
                }
                window.layout();
            }
            WM_CLOSE => {
                if window.file_pending.get() {
                    return LRESULT(0);
                }
                if window.editor.try_borrow().is_err() {
                    return LRESULT(0);
                }
                if window.controller.borrow().is_some() && !window.initializing.get() {
                    window.post(json!({ "kind": "close" }));
                } else {
                    let _ = DestroyWindow(hwnd);
                }
            }
            WM_SETTINGS_CLOSE => {
                let _ = DestroyWindow(hwnd);
            }
            WM_NCDESTROY => {
                let _ = KillTimer(hwnd, TIMER_STARTUP);
                ACTIVE.with(|active| active.set(HWND::default()));
                REQUESTS.with_borrow_mut(|requests| requests.retain(|(sender, _)| *sender != hwnd));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let window = Box::from_raw(window as *const SettingsWindow as *mut SettingsWindow);
                if let Some(controller) = window.controller.borrow_mut().take() {
                    let _ = controller.Close();
                }
                if !window.icon.0.is_null() {
                    let _ = DestroyIcon(window.icon);
                }
                drop(window);
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    }))
    .unwrap_or_else(|_| {
        log_error("settings", "Caught panic in settings window");
        DefWindowProcW(hwnd, msg, wp, lp)
    })
}
