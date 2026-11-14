pub(super) unsafe fn open_file_dialog(
    hwnd: windows::Win32::Foundation::HWND,
    pick_folders: bool,
    filters: &[(&str, &str)],
) -> windows::core::Result<Option<String>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

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

pub(super) unsafe fn save_file_dialog(
    hwnd: windows::Win32::Foundation::HWND,
    default_name: &str,
    default_ext: &str,
    filters: &[(&str, &str)],
) -> windows::core::Result<Option<String>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        SIGDN_FILESYSPATH,
    };

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
