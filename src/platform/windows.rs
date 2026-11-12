//! Win32 implementation of `WindowSystem`.
//!
//! Strictly adheres to Section 4.2, 5.3, and 7.2 of spec.md:
//! - Never calls synchronous `SendMessageW` on foreign windows.
//! - Uses `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` + `GetProcessTimes`
//!   and immediately closes the process handle.
//! - Filters out cloaked windows (`DWMWA_CLOAKED`), other virtual desktops (`IVirtualDesktopManager`),
//!   owned windows (`GW_OWNER`), and our own process windows.
//! - Uses `SHGetPropertyStoreForWindow` (`PKEY_AppUserModel_ID`) for packaged apps (`ApplicationFrameHost.exe`).
//! - Attributes minimized windows to their monitor using `WINDOWPLACEMENT.rcNormalPosition` / `MonitorFromWindow`.

use crate::config::RectData;
use crate::platform::monitors::{enumerate_connected_monitors, find_best_monitor_for_rect};
use crate::platform::{MonitorInfoData, PlatformError, ShowCmd, WindowInstance, WindowSystem};
use std::cell::RefCell;
use std::collections::HashMap;

pub struct Win32WindowSystem {
    own_pid: u32,
    #[cfg(windows)]
    vdm: VirtualDesktopChecker,
    cached_monitors: RefCell<Vec<MonitorInfoData>>,
}

impl Default for Win32WindowSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32WindowSystem {
    pub fn new() -> Self {
        Self {
            own_pid: std::process::id(),
            #[cfg(windows)]
            vdm: VirtualDesktopChecker::new(),
            cached_monitors: RefCell::new(Vec::new()),
        }
    }

    fn refresh_monitors(&self) -> Vec<MonitorInfoData> {
        let monitors = enumerate_connected_monitors();
        *self.cached_monitors.borrow_mut() = monitors.clone();
        monitors
    }
}

#[cfg(windows)]
impl WindowSystem for Win32WindowSystem {
    fn enumerate_windows(&self) -> Vec<WindowInstance> {
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

        let monitors = self.refresh_monitors();

        struct EnumCtx<'a> {
            own_pid: u32,
            monitors: &'a [MonitorInfoData],
            vdm: &'a VirtualDesktopChecker,
            pid_cache: HashMap<u32, (String, u64, bool)>,
            results: Vec<WindowInstance>,
        }

        unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut EnumCtx<'_>);
            if let Some(mut instance) = inspect_hwnd(
                hwnd,
                ctx.own_pid,
                ctx.monitors,
                Some(ctx.vdm),
                Some(&mut ctx.pid_cache),
                true,
            ) {
                instance.z_order = ctx.results.len();
                ctx.results.push(instance);
            }
            BOOL(1)
        }

        let mut ctx = EnumCtx {
            own_pid: self.own_pid,
            monitors: &monitors,
            vdm: &self.vdm,
            pid_cache: HashMap::with_capacity(32),
            results: Vec::with_capacity(32),
        };

        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut EnumCtx<'_> as isize));
        }

        ctx.results
    }

    fn query_window(&self, hwnd_val: u64) -> Option<WindowInstance> {
        use windows::Win32::Foundation::HWND;
        if self.cached_monitors.borrow().is_empty() {
            let _ = self.refresh_monitors();
        }
        let monitors = self.cached_monitors.borrow();
        let hwnd = HWND(hwnd_val as usize as *mut std::ffi::c_void);
        unsafe { inspect_hwnd(hwnd, self.own_pid, &monitors, None, None, false) }
    }

    fn show_window_async(&mut self, hwnd_val: u64, cmd: ShowCmd) -> Result<(), PlatformError> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            IsWindow, ShowWindow, ShowWindowAsync, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINNOACTIVE,
            SW_SHOWNOACTIVATE,
        };

        let hwnd = HWND(hwnd_val as usize as *mut std::ffi::c_void);
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return Err(PlatformError::WindowNotFound(hwnd_val));
            }

            let n_cmd = match cmd {
                ShowCmd::MinimizeNoActivate => SW_SHOWMINNOACTIVE,
                ShowCmd::MinimizeActivateNext => SW_MINIMIZE,
                ShowCmd::RestoreNoActivate => SW_SHOWNOACTIVATE,
                ShowCmd::RestoreActivate => SW_RESTORE,
            };

            // For restore operations, use ShowWindow synchronously so the window immediately
            // exits iconic state before subsequent bring_to_top / set_foreground_window calls.
            // For minimize operations, use ShowWindowAsync to prevent blocking if an app is slow.
            match cmd {
                ShowCmd::RestoreActivate | ShowCmd::RestoreNoActivate => {
                    let _ = ShowWindow(hwnd, n_cmd);
                }
                _ => {
                    let _ = ShowWindowAsync(hwnd, n_cmd);
                }
            }
            Ok(())
        }
    }

    fn bring_to_top(&mut self, hwnd_val: u64) -> bool {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, IsIconic, SetWindowPos, ShowWindow, HWND_TOP, SW_RESTORE,
            SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        };
        let hwnd = HWND(hwnd_val as usize as *mut std::ffi::c_void);
        unsafe {
            // If the window is still iconic, restore it immediately so SetWindowPos succeeds
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            BringWindowToTop(hwnd).is_ok()
        }
    }

    fn get_foreground_window(&self) -> Option<u64> {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                None
            } else {
                Some(hwnd.0 as usize as u64)
            }
        }
    }

    fn set_foreground_window(&mut self, hwnd_val: u64) -> bool {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
            SetWindowPos, HWND_TOP, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        };

        let target_hwnd = HWND(hwnd_val as usize as *mut std::ffi::c_void);
        unsafe {
            if windows::Win32::UI::WindowsAndMessaging::IsIconic(target_hwnd).as_bool() {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                    target_hwnd,
                    windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                );
            }
            let _ = SetWindowPos(
                target_hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = BringWindowToTop(target_hwnd);

            let fg_hwnd = GetForegroundWindow();
            if fg_hwnd == target_hwnd {
                return true;
            }

            let cur_thread_id = GetCurrentThreadId();
            let fg_thread_id = if !fg_hwnd.0.is_null() {
                GetWindowThreadProcessId(fg_hwnd, None)
            } else {
                0
            };
            let target_thread_id = GetWindowThreadProcessId(target_hwnd, None);

            if fg_thread_id != 0 && fg_thread_id != cur_thread_id {
                let _ = AttachThreadInput(cur_thread_id, fg_thread_id, true);
                if target_thread_id != 0 && target_thread_id != cur_thread_id {
                    let _ = AttachThreadInput(cur_thread_id, target_thread_id, true);
                }

                let ok = SetForegroundWindow(target_hwnd).as_bool();
                let _ = BringWindowToTop(target_hwnd);

                if target_thread_id != 0 && target_thread_id != cur_thread_id {
                    let _ = AttachThreadInput(cur_thread_id, target_thread_id, false);
                }
                let _ = AttachThreadInput(cur_thread_id, fg_thread_id, false);
                ok
            } else {
                SetForegroundWindow(target_hwnd).as_bool()
            }
        }
    }

    fn set_window_placement(
        &mut self,
        hwnd_val: u64,
        normal_rect: RectData,
        maximized: bool,
    ) -> Result<(), PlatformError> {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowPlacement, IsIconic, IsWindow, SetWindowPlacement, SW_SHOWMAXIMIZED,
            SW_SHOWNORMAL, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
        };

        let hwnd = HWND(hwnd_val as usize as *mut std::ffi::c_void);
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return Err(PlatformError::WindowNotFound(hwnd_val));
            }
            let mut wp = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            GetWindowPlacement(hwnd, &mut wp)
                .map_err(|e| PlatformError::OsError(format!("GetWindowPlacement: {}", e)))?;

            wp.rcNormalPosition = RECT {
                left: normal_rect.left,
                top: normal_rect.top,
                right: normal_rect.right,
                bottom: normal_rect.bottom,
            };

            if IsIconic(hwnd).as_bool() {
                if maximized {
                    wp.flags |= WPF_RESTORETOMAXIMIZED;
                } else {
                    wp.flags &= !WPF_RESTORETOMAXIMIZED;
                }
            } else {
                wp.showCmd = if maximized {
                    SW_SHOWMAXIMIZED.0 as u32
                } else {
                    SW_SHOWNORMAL.0 as u32
                };
            }

            SetWindowPlacement(hwnd, &wp)
                .map_err(|e| PlatformError::OsError(format!("SetWindowPlacement: {}", e)))?;
            Ok(())
        }
    }

    fn monitors(&self) -> Vec<MonitorInfoData> {
        self.refresh_monitors()
    }
}

#[inline]
fn ends_with_ignore_ascii_case(haystack: &str, suffix: &str) -> bool {
    haystack.len() >= suffix.len()
        && haystack.as_bytes()[haystack.len() - suffix.len()..]
            .eq_ignore_ascii_case(suffix.as_bytes())
}

#[cfg(windows)]
unsafe fn inspect_hwnd(
    hwnd: windows::Win32::Foundation::HWND,
    own_pid: u32,
    monitors: &[MonitorInfoData],
    vdm: Option<&VirtualDesktopChecker>,
    pid_cache: Option<&mut HashMap<u32, (String, u64, bool)>>,
    require_visible_top_level: bool,
) -> Option<WindowInstance> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindow, GetWindowLongW, GetWindowPlacement, GetWindowRect,
        GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow,
        IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, SW_SHOWMAXIMIZED,
        WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED, WS_CHILD, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };

    if !IsWindow(hwnd).as_bool() {
        return None;
    }

    let is_visible = IsWindowVisible(hwnd).as_bool();
    if require_visible_top_level && !is_visible {
        return None;
    }

    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if (style & WS_CHILD.0) != 0 {
        return None;
    }

    // Per Section 4.3: Owned windows are excluded from standalone registration; OS follows owner
    let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
    if require_visible_top_level && !owner.0.is_null() {
        return None;
    }

    // Check DWMWA_CLOAKED (exclude background UWP frames / suspended windows)
    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut std::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
    {
        return None;
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 || pid == own_pid {
        return None;
    }

    // Exclude windows on other virtual desktops
    if let Some(checker) = vdm {
        if !checker.is_window_on_current_desktop(hwnd) {
            return None;
        }
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let is_tool_window = (ex_style & WS_EX_TOOLWINDOW.0) != 0;
    let is_app_window = (ex_style & WS_EX_APPWINDOW.0) != 0;
    let is_no_activate = (ex_style & WS_EX_NOACTIVATE.0) != 0;

    // Exclude non-app tool/noactivate overlay windows during general enumeration unless they have a title
    let has_taskbar_button = (!is_tool_window && owner.0.is_null()) || is_app_window;

    let mut title_buf = [0u16; 512];
    let title_len = GetWindowTextW(hwnd, &mut title_buf);
    let title = if title_len > 0 {
        String::from_utf16_lossy(&title_buf[..title_len as usize])
    } else {
        String::new()
    };

    if require_visible_top_level && title.is_empty() && (is_tool_window || is_no_activate) {
        return None;
    }

    let mut class_buf = [0u16; 256];
    let class_len = GetClassNameW(hwnd, &mut class_buf);
    let window_class = if class_len > 0 {
        String::from_utf16_lossy(&class_buf[..class_len as usize])
    } else {
        String::new()
    };

    // Exclude Windows shell desktop / taskbar chrome classes
    if matches!(
        window_class.as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return None;
    }

    let is_iconic = IsIconic(hwnd).as_bool();
    let is_enabled = IsWindowEnabled(hwnd).as_bool();
    let is_hung = IsHungAppWindow(hwnd).as_bool();

    let mut wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let _ = GetWindowPlacement(hwnd, &mut wp);

    let normal_rect = RectData {
        left: wp.rcNormalPosition.left,
        top: wp.rcNormalPosition.top,
        right: wp.rcNormalPosition.right,
        bottom: wp.rcNormalPosition.bottom,
    };

    let mut win_rc = RECT::default();
    let _ = GetWindowRect(hwnd, &mut win_rc);
    let window_rect = RectData {
        left: win_rc.left,
        top: win_rc.top,
        right: win_rc.right,
        bottom: win_rc.bottom,
    };

    let is_maximized = if is_iconic {
        (wp.flags.0 & WPF_RESTORETOMAXIMIZED.0) != 0
    } else {
        wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
    };

    // Per Section 4.2 #6: When minimized, use normal_rect (or MonitorFromWindow pre-minimized rect)
    // instead of the off-screen (-32000) window_rect.
    let attribution_rect = if is_iconic || window_rect.left <= -30000 {
        &normal_rect
    } else {
        &window_rect
    };

    let monitor_id = find_best_monitor_for_rect(attribution_rect, monitors)
        .map(|m| m.monitor_id.clone())
        .unwrap_or_else(|| "UNKNOWN-MONITOR".to_string());

    let (exe_path, process_start_time, is_elevated) = match pid_cache {
        Some(cache) => cache
            .entry(pid)
            .or_insert_with(|| query_process_identity(pid))
            .clone(),
        None => query_process_identity(pid),
    };

    let aumid = if ends_with_ignore_ascii_case(&exe_path, "applicationframehost.exe") {
        query_window_aumid(hwnd)
    } else {
        None
    };

    Some(WindowInstance {
        hwnd: hwnd.0 as usize as u64,
        pid,
        process_start_time,
        exe_path,
        window_class,
        title,
        aumid,
        monitor_id,
        is_visible,
        is_iconic,
        is_enabled,
        is_hung,
        is_maximized,
        has_taskbar_button,
        is_elevated,
        normal_rect,
        window_rect,
        z_order: 0,
    })
}

#[cfg(windows)]
unsafe fn query_process_identity(pid: u32) -> (String, u64, bool) {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, GetLastError};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let h_proc = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        Ok(h) => h,
        Err(_) => {
            let is_denied = GetLastError() == ERROR_ACCESS_DENIED;
            return (String::new(), 0, is_denied);
        }
    };

    let mut path_buf = [0u16; 1024];
    let mut size = path_buf.len() as u32;
    let exe_path = if QueryFullProcessImageNameW(
        h_proc,
        PROCESS_NAME_WIN32,
        PWSTR(path_buf.as_mut_ptr()),
        &mut size,
    )
    .is_ok()
    {
        String::from_utf16_lossy(&path_buf[..size as usize])
    } else {
        String::new()
    };

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let start_time = if GetProcessTimes(h_proc, &mut creation, &mut exit, &mut kernel, &mut user).is_ok()
    {
        ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64)
    } else {
        0
    };

    let mut is_elevated = false;
    let mut h_token = Default::default();
    if OpenProcessToken(h_proc, TOKEN_QUERY, &mut h_token).is_ok() {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        if GetTokenInformation(
            h_token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok()
        {
            is_elevated = elevation.TokenIsElevated != 0;
        }
        let _ = CloseHandle(h_token);
    }

    let _ = CloseHandle(h_proc);
    (exe_path, start_time, is_elevated)
}

/// Query `PKEY_AppUserModel_ID` via `SHGetPropertyStoreForWindow` for packaged apps (Section 4.2 #9).
#[cfg(windows)]
unsafe fn query_window_aumid(hwnd: windows::Win32::Foundation::HWND) -> Option<String> {
    use windows::core::GUID;
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow, PROPERTYKEY,
    };

    // PKEY_AppUserModel_ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 5,
    };

    let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
    let prop_val = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
    let s = prop_val.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Helper wrapping COM `IVirtualDesktopManager` to filter out windows on other virtual desktops (Section 4.2 #7).
/// Initialized once per `Win32WindowSystem` instead of per enumeration.
#[cfg(windows)]
struct VirtualDesktopChecker {
    manager: Option<windows::Win32::UI::Shell::IVirtualDesktopManager>,
    com_initialized: bool,
}

#[cfg(windows)]
impl VirtualDesktopChecker {
    fn new() -> Self {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let com_initialized = hr.is_ok();
            let manager: Option<IVirtualDesktopManager> =
                CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER).ok();
            Self {
                manager,
                com_initialized,
            }
        }
    }

    unsafe fn is_window_on_current_desktop(&self, hwnd: windows::Win32::Foundation::HWND) -> bool {
        if let Some(ref mgr) = self.manager {
            if let Ok(on_current) = mgr.IsWindowOnCurrentVirtualDesktop(hwnd) {
                return on_current.as_bool();
            }
        }
        true
    }
}

#[cfg(windows)]
impl Drop for VirtualDesktopChecker {
    fn drop(&mut self) {
        self.manager = None;
        if self.com_initialized {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}
