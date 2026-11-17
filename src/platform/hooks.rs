//! WinEvent hooks (`SetWinEventHook`) for:
//! 1. `EVENT_SYSTEM_MINIMIZESTART` ..= `EVENT_SYSTEM_MINIMIZEEND` (minimize/restore tracking)
//! 2. `EVENT_SYSTEM_FOREGROUND` (foreground window changes for low-overhead topmost maintenance)
//!
//! Per Section 5.3 & 7.3 of spec.md:
//! - Uses `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS` with narrow event ranges
//!   (kept as separate hooks so intermediate unrelated system events are never dispatched).
//! - Callbacks do minimal work: update lock-free/bounded state and post a coalesced
//!   `WM_APP_*` message to the launcher HWND so the message loop processes it asynchronously.
//! - Protected by `std::panic::catch_unwind` at the FFI boundary.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Custom window message posted to the launcher window when a minimize WinEvent arrives.
pub const WM_APP_WINEVENT: u32 = 0x8000 + 1; // WM_APP + 1

/// Custom window message posted (coalesced) when the OS foreground window changes.
pub const WM_APP_FOREGROUND_CHANGED: u32 = 0x8000 + 15; // WM_APP + 15

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizeEventKind {
    MinimizeStart,
    MinimizeEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinimizeEvent {
    pub hwnd: u64,
    pub kind: MinimizeEventKind,
    pub timestamp_ms: u64,
}

static EVENT_QUEUE: OnceLock<Mutex<VecDeque<MinimizeEvent>>> = OnceLock::new();
static NOTIFY_HWND: AtomicUsize = AtomicUsize::new(0);
static FOREGROUND_NOTIFY_HWND: AtomicUsize = AtomicUsize::new(0);
static FOREGROUND_PENDING: AtomicBool = AtomicBool::new(false);

const MAX_QUEUED_EVENTS: usize = 256;

fn queue() -> &'static Mutex<VecDeque<MinimizeEvent>> {
    EVENT_QUEUE.get_or_init(|| Mutex::new(VecDeque::with_capacity(64)))
}

/// Push a minimize event (also usable from tests).
/// Returns `true` if the queue transitioned from empty to non-empty (so the caller can coalesce wakeups).
pub fn push_minimize_event(event: MinimizeEvent) -> bool {
    if let Ok(mut guard) = queue().lock() {
        let was_empty = guard.is_empty();
        if guard.len() >= MAX_QUEUED_EVENTS {
            guard.pop_front();
        }
        guard.push_back(event);
        was_empty
    } else {
        false
    }
}

/// Drain all queued minimize events.
pub fn drain_minimize_events() -> Vec<MinimizeEvent> {
    if let Ok(mut guard) = queue().lock() {
        guard.drain(..).collect()
    } else {
        Vec::new()
    }
}

pub struct WinEventHookGuard {
    #[cfg(windows)]
    hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
}

impl WinEventHookGuard {
    #[cfg(windows)]
    pub fn install(notify_hwnd: windows::Win32::Foundation::HWND) -> Option<Self> {
        use windows::Win32::UI::Accessibility::SetWinEventHook;
        use windows::Win32::UI::WindowsAndMessaging::{
            EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, WINEVENT_OUTOFCONTEXT,
            WINEVENT_SKIPOWNPROCESS,
        };

        NOTIFY_HWND.store(notify_hwnd.0 as usize, Ordering::Release);

        unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_MINIMIZESTART,
                EVENT_SYSTEM_MINIMIZEEND,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if hook.is_invalid() {
                None
            } else {
                Some(Self { hook })
            }
        }
    }
}

#[cfg(windows)]
impl Drop for WinEventHookGuard {
    fn drop(&mut self) {
        use windows::Win32::UI::Accessibility::UnhookWinEvent;
        NOTIFY_HWND.store(0, Ordering::Release);
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWinEvent(self.hook);
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        PostMessageW, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, OBJID_WINDOW,
    };

    let _ = std::panic::catch_unwind(|| {
        // Only handle top-level window object events (OBJID_WINDOW == 0, CHILDID_SELF == 0)
        if hwnd.0.is_null() || id_object != OBJID_WINDOW.0 || id_child != 0 {
            return;
        }

        let kind = if event == EVENT_SYSTEM_MINIMIZESTART {
            MinimizeEventKind::MinimizeStart
        } else if event == EVENT_SYSTEM_MINIMIZEEND {
            MinimizeEventKind::MinimizeEnd
        } else {
            return;
        };

        let should_notify = push_minimize_event(MinimizeEvent {
            hwnd: hwnd.0 as usize as u64,
            kind,
            timestamp_ms: crate::diagnostics::current_unix_ms(),
        });

        if should_notify {
            let target_raw = NOTIFY_HWND.load(Ordering::Acquire);
            if target_raw != 0 {
                let target_hwnd = HWND(target_raw as *mut std::ffi::c_void);
                let _ = PostMessageW(Some(target_hwnd), WM_APP_WINEVENT, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// Consume and clear the coalesced foreground-change flag.
pub fn take_foreground_change_pending() -> bool {
    FOREGROUND_PENDING.swap(false, Ordering::AcqRel)
}

/// Separate lightweight hook strictly for `EVENT_SYSTEM_FOREGROUND` (`0x0003..=0x0003`).
/// Installed only while always-on-top bar mode is active so disabled states incur zero callback overhead.
pub struct ForegroundHookGuard {
    #[cfg(windows)]
    hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
}

impl ForegroundHookGuard {
    #[cfg(windows)]
    pub fn install(notify_hwnd: windows::Win32::Foundation::HWND) -> Option<Self> {
        use windows::Win32::UI::Accessibility::SetWinEventHook;
        use windows::Win32::UI::WindowsAndMessaging::{
            EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
        };

        FOREGROUND_PENDING.store(false, Ordering::Release);
        FOREGROUND_NOTIFY_HWND.store(notify_hwnd.0 as usize, Ordering::Release);

        unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if hook.is_invalid() {
                FOREGROUND_NOTIFY_HWND.store(0, Ordering::Release);
                None
            } else {
                Some(Self { hook })
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ForegroundHookGuard {
    fn drop(&mut self) {
        use windows::Win32::UI::Accessibility::UnhookWinEvent;
        FOREGROUND_NOTIFY_HWND.store(0, Ordering::Release);
        FOREGROUND_PENDING.store(false, Ordering::Release);
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWinEvent(self.hook);
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn foreground_event_proc(
    _h_win_event_hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        PostMessageW, EVENT_SYSTEM_FOREGROUND, OBJID_WINDOW,
    };

    let _ = std::panic::catch_unwind(|| {
        if event != EVENT_SYSTEM_FOREGROUND
            || hwnd.0.is_null()
            || id_object != OBJID_WINDOW.0
            || id_child != 0
        {
            return;
        }

        // Coalesce rapid foreground transitions into at most one queued message.
        if !FOREGROUND_PENDING.swap(true, Ordering::AcqRel) {
            let target_raw = FOREGROUND_NOTIFY_HWND.load(Ordering::Acquire);
            if target_raw != 0 {
                let target_hwnd = HWND(target_raw as *mut std::ffi::c_void);
                if PostMessageW(
                    Some(target_hwnd),
                    WM_APP_FOREGROUND_CHANGED,
                    WPARAM(0),
                    LPARAM(0),
                )
                .is_err()
                {
                    FOREGROUND_PENDING.store(false, Ordering::Release);
                }
            } else {
                FOREGROUND_PENDING.store(false, Ordering::Release);
            }
        }
    });
}
