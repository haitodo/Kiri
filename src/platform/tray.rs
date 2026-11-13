//! System tray (Notification Area) icon management (`Shell_NotifyIconW`)
//! with automatic re-registration on Explorer restart (`TaskbarCreated`).
//!
//! Per Section 7.2 & 8.2 (AT-28) of spec.md:
//! - Registers a notification icon so the user always has a recovery path ("Restore All", "Settings", "Exit").
//! - Registers the `"TaskbarCreated"` window message and re-adds the tray icon if `explorer.exe` restarts.

/// Custom callback message sent to the launcher HWND by the tray icon.
pub const WM_APP_TRAYICON: u32 = 0x8000 + 2; // WM_APP + 2
const TRAY_ICON_ID: u32 = 1001;

pub struct TrayIcon {
    #[cfg(windows)]
    owner_hwnd: windows::Win32::Foundation::HWND,
    #[cfg(windows)]
    custom_hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
    taskbar_created_msg: u32,
    registered: bool,
}

impl TrayIcon {
    #[cfg(windows)]
    pub fn new(owner_hwnd: windows::Win32::Foundation::HWND) -> Self {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;

        let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let custom_hicon = crate::launcher::paint::create_kiri_hicon(32).unwrap_or_default();
        let mut tray = Self {
            owner_hwnd,
            custom_hicon,
            taskbar_created_msg,
            registered: false,
        };
        tray.add_or_readd();
        tray
    }

    pub fn taskbar_created_message_id(&self) -> u32 {
        self.taskbar_created_msg
    }

    /// Add or re-add the tray icon (called at startup and whenever `TaskbarCreated` is received).
    #[cfg(windows)]
    pub fn add_or_readd(&mut self) {
        use windows::core::w;
        use windows::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_MODIFY,
            NOTIFYICONDATAW,
        };
        use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION};

        unsafe {
            let hicon = if !self.custom_hicon.0.is_null() {
                self.custom_hicon
            } else {
                LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
            };
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.owner_hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_APP_TRAYICON,
                hIcon: hicon,
                ..Default::default()
            };

            let tip = w!("Kiri - 作業シーン切替ランチャー");
            let slice = tip.as_wide();
            let copy_len = slice.len().min(nid.szTip.len() - 1);
            nid.szTip[..copy_len].copy_from_slice(&slice[..copy_len]);

            if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                self.registered = true;
            } else {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
                self.registered = true;
            }
        }
    }

    /// Show a non-activating Windows tray balloon/toast notification.
    #[cfg(windows)]
    pub fn show_notification(&self, title: &str, message: &str, is_warning: bool) {
        use windows::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING, NIM_MODIFY,
            NOTIFYICONDATAW,
        };

        if !self.registered {
            return;
        }

        fn copy_utf16_into(dst: &mut [u16], text: &str) {
            if dst.is_empty() {
                return;
            }
            dst.fill(0);
            let max_len = dst.len() - 1;
            for (i, unit) in text.encode_utf16().take(max_len).enumerate() {
                dst[i] = unit;
            }
        }

        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.owner_hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_INFO,
                dwInfoFlags: if is_warning {
                    NIIF_WARNING | NIIF_NOSOUND
                } else {
                    NIIF_INFO | NIIF_NOSOUND
                },
                ..Default::default()
            };
            copy_utf16_into(&mut nid.szInfoTitle, title);
            copy_utf16_into(&mut nid.szInfo, message);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    #[cfg(windows)]
    pub fn remove(&mut self) {
        use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
        if !self.registered {
            return;
        }
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.owner_hwnd,
                uID: TRAY_ICON_ID,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.registered = false;
    }
}

#[cfg(windows)]
impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
        if !self.custom_hicon.0.is_null() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(self.custom_hicon);
            }
            self.custom_hicon = windows::Win32::UI::WindowsAndMessaging::HICON::default();
        }
    }
}
