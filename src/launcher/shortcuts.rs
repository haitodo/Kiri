use crate::config::{Shortcut, ShortcutKind};
use std::fs;
use std::mem::size_of;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, SW_SHOWNORMAL,
};

pub(super) fn open(shortcut: Shortcut) {
    // Network paths and shell handlers can block. Keep the launcher message loop responsive.
    let result = std::thread::Builder::new().spawn(move || {
        if let Err(error) = open_target(&shortcut) {
            show_error(&format!(
                "「{}」を開けませんでした。\n{}",
                shortcut.name, error
            ));
        }
    });
    if let Err(error) = result {
        show_error(&format!("ショートカットを開けませんでした。\n{error}"));
    }
}

fn open_target(shortcut: &Shortcut) -> Result<(), String> {
    shortcut.validate()?;
    if shortcut.kind != ShortcutKind::Website {
        let metadata = fs::metadata(&shortcut.target)
            .map_err(|error| format!("場所を確認できませんでした。\n{error}"))?;
        let matches_kind = match shortcut.kind {
            ShortcutKind::Folder => metadata.is_dir(),
            ShortcutKind::File => metadata.is_file(),
            ShortcutKind::Website => unreachable!(),
        };
        if !matches_kind {
            return Err(format!(
                "指定した場所は{}として開けません。",
                shortcut.kind.label()
            ));
        }
    }

    let target: Vec<u16> = shortcut
        .target
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|error| error.to_string())?;
        let result = ShellExecuteExW(&mut SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            lpVerb: w!("open"),
            lpFile: PCWSTR(target.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        });
        CoUninitialize();
        result.map_err(|error| error.to_string())
    }
}

fn show_error(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            w!("Kiri"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}
