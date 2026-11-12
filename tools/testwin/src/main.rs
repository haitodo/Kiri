//! `testwin` — Acceptance test dummy window utility (Section 7.1 of spec.md).
//!
//! Reproduces edge-case window behaviors without requiring commercial trading terminals:
//! - `--mode normal` (default): Standard top-level window
//! - `--mode multi`: Spawns 2 windows with the exact same EXE and window class
//! - `--mode modal`: Opens a modal dialog that disables the owner window (`IsWindowEnabled == false`, AT-26)
//! - `--mode tray-hide`: Hides the window (`SW_HIDE`) when minimized to simulate tray-minimizing apps (Section 8.2)
//! - `--mode no-taskbar`: Tool window without a taskbar button (`WS_EX_TOOLWINDOW`, Section 2.4)
//! - `--mode maximized`: Starts maximized (`WS_MAXIMIZE`, AT-24)
//! - `--mode dynamic-title`: Changes its window title every second (simulating live ticker / MT5 chart title)
//! - `--mode hung`: Stops processing window messages for 15 seconds on right-click (AT-14)

use std::env;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static HIDE_ON_MINIMIZE: AtomicBool = AtomicBool::new(false);
static SIMULATE_HUNG_ON_RCLICK: AtomicBool = AtomicBool::new(false);
static DYNAMIC_TITLE_TICK: AtomicU32 = AtomicU32::new(0);

const TIMER_DYNAMIC_TITLE: usize = 901;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mode = parse_arg(&args, "--mode").unwrap_or_else(|| "normal".to_string());
    let base_title =
        parse_arg(&args, "--title").unwrap_or_else(|| format!("TestWin [{}]", mode));

    #[cfg(windows)]
    unsafe {
        run_testwin(&mode, &base_title);
    }
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

#[cfg(windows)]
unsafe fn run_testwin(mode: &str, base_title: &str) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, LoadCursorW, MessageBoxW, RegisterClassW,
        SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDC_ARROW,
        MB_ICONWARNING, MB_OK, MSG, SW_SHOW, SW_SHOWMAXIMIZED, WINDOW_EX_STYLE, WNDCLASSW,
        WS_EX_TOOLWINDOW, WS_MAXIMIZE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    let hinstance = GetModuleHandleW(None).unwrap();
    let class_name = w!("KiriDummyTestWinClass");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(testwin_wndproc),
        hInstance: hinstance.into(),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: class_name,
        ..Default::default()
    };
    let _ = RegisterClassW(&wc);

    HIDE_ON_MINIMIZE.store(mode == "tray-hide", Ordering::Relaxed);
    SIMULATE_HUNG_ON_RCLICK.store(mode == "hung", Ordering::Relaxed);

    let ex_style = if mode == "no-taskbar" {
        WS_EX_TOOLWINDOW
    } else {
        WINDOW_EX_STYLE(0)
    };

    let mut style = WS_OVERLAPPEDWINDOW | WS_VISIBLE;
    if mode == "maximized" {
        style |= WS_MAXIMIZE;
    }

    let title_w: Vec<u16> = base_title.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        ex_style,
        class_name,
        PCWSTR(title_w.as_ptr()),
        style,
        200,
        200,
        640,
        420,
        HWND::default(),
        None,
        hinstance,
        None,
    )
    .expect("CreateWindowExW failed");

    let _ = ShowWindow(
        hwnd,
        if mode == "maximized" {
            SW_SHOWMAXIMIZED
        } else {
            SW_SHOW
        },
    );

    if mode == "multi" {
        let title2: Vec<u16> = format!("{} #2", base_title)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let _ = CreateWindowExW(
            ex_style,
            class_name,
            PCWSTR(title2.as_ptr()),
            style,
            260,
            260,
            640,
            420,
            HWND::default(),
            None,
            hinstance,
            None,
        );
    }

    if mode == "dynamic-title" {
        let _ = SetTimer(hwnd, TIMER_DYNAMIC_TITLE, 1000, None);
    }

    if mode == "modal" {
        let _ = MessageBoxW(
            hwnd,
            w!("モーダル注文確認ダイアログのシミュレーション中（親ウィンドウは IsWindowEnabled == false になります）"),
            w!("TestWin Modal Dialog"),
            MB_OK | MB_ICONWARNING,
        );
    }

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

#[cfg(windows)]
unsafe extern "system" fn testwin_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, PostQuitMessage, SetWindowTextW, ShowWindow, SIZE_MINIMIZED, SW_HIDE,
        WM_DESTROY, WM_RBUTTONUP, WM_SIZE, WM_TIMER,
    };

    match msg {
        WM_SIZE => {
            if wparam.0 as u32 == SIZE_MINIMIZED && HIDE_ON_MINIMIZE.load(Ordering::Relaxed) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_RBUTTONUP => {
            if SIMULATE_HUNG_ON_RCLICK.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(15));
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_DYNAMIC_TITLE {
                let tick = DYNAMIC_TITLE_TICK.fetch_add(1, Ordering::Relaxed) + 1;
                let dynamic = format!("12345678: MetaTrader 5 - USDJPY,M1 [Tick #{}]", tick);
                let wide: Vec<u16> = dynamic.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
