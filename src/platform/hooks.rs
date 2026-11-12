//! WinEvent hook (`SetWinEventHook`) for `EVENT_SYSTEM_MINIMIZESTART` and `EVENT_SYSTEM_MINIMIZEEND`.
//!
//! Per Section 5.3 & 7.3 of spec.md:
//! - Uses `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS` restricted strictly to
//!   `EVENT_SYSTEM_MINIMIZESTART` (0x0016) through `EVENT_SYSTEM_MINIMIZEEND` (0x0017).
//! - The callback does minimal work: pushes the event into a thread-safe queue and posts
//!   `WM_APP_WINEVENT` to the launcher HWND so the message loop processes it asynchronously.
//! - Protected by `std::panic::catch_unwind` at the FFI boundary.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Custom window message posted to the launcher window when a WinEvent arrives.
pub const WM_APP_WINEVENT: u32 = 0x8000 + 1; // WM_APP + 1

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
                let _ = PostMessageW(target_hwnd, WM_APP_WINEVENT, WPARAM(0), LPARAM(0));
            }
        }
    });
}
