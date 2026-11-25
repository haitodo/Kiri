use std::env;
use std::fs;
use std::path::PathBuf;

fn write_res_header(
    out: &mut Vec<u8>,
    data_size: u32,
    type_id: u16,
    name_id: u16,
    memory_flags: u16,
    language_id: u16,
) {
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&type_id.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&name_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&memory_flags.to_le_bytes());
    out.extend_from_slice(&language_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
}

fn pad_to_4_bytes(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// Convert a `.ico` file into a Win32 `.res` binary containing `RT_ICON` entries and
/// `RT_GROUP_ICON` resource ID `1` so Windows Explorer and the taskbar recognize the app icon.
fn compile_ico_to_res(ico: &[u8]) -> Result<Vec<u8>, String> {
    if ico.len() < 6 {
        return Err("icon.ico is too small".into());
    }
    let reserved = u16::from_le_bytes([ico[0], ico[1]]);
    let ico_type = u16::from_le_bytes([ico[2], ico[3]]);
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if reserved != 0 || ico_type != 1 || count == 0 {
        return Err("icon.ico has invalid ICONDIR header".into());
    }
    let dir_end = 6 + count * 16;
    if ico.len() < dir_end {
        return Err("icon.ico directory truncated".into());
    }

    let mut res = Vec::with_capacity(ico.len() + 256);
    // Standard 32-byte empty resource header required at the start of a Win32 `.res` file.
    write_res_header(&mut res, 0, 0, 0, 0, 0);

    let mut group_payload = Vec::with_capacity(6 + count * 14);
    group_payload.extend_from_slice(&0u16.to_le_bytes());
    group_payload.extend_from_slice(&1u16.to_le_bytes());
    group_payload.extend_from_slice(&(count as u16).to_le_bytes());

    for idx in 0..count {
        let entry_start = 6 + idx * 16;
        let entry = &ico[entry_start..entry_start + 16];
        let bytes_in_res = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let image_offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        let image_end = image_offset
            .checked_add(bytes_in_res)
            .ok_or_else(|| "icon.ico entry offset overflow".to_string())?;
        if image_end > ico.len() {
            return Err("icon.ico image payload out of bounds".into());
        }

        let icon_id = u16::try_from(idx + 1).map_err(|_| "too many icon entries".to_string())?;
        let payload = &ico[image_offset..image_end];
        write_res_header(&mut res, payload.len() as u32, 3 /* RT_ICON */, icon_id, 0x1010, 0x0000);
        res.extend_from_slice(payload);
        pad_to_4_bytes(&mut res);

        // GRPICONDIRENTRY copies the first 12 bytes of ICONDIRENTRY followed by the resource ID.
        group_payload.extend_from_slice(&entry[..12]);
        group_payload.extend_from_slice(&icon_id.to_le_bytes());
    }

    write_res_header(
        &mut res,
        group_payload.len() as u32,
        14, /* RT_GROUP_ICON */
        1,
        0x1030,
        0x0000,
    );
    res.extend_from_slice(&group_payload);
    pad_to_4_bytes(&mut res);
    Ok(res)
}

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=logo/icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let manifest_path = manifest_dir.join("app.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest_path.display()
        );

        let ico_path = manifest_dir.join("logo").join("icon.ico");
        let ico_bytes = fs::read(&ico_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", ico_path.display()));
        let res_bytes = compile_ico_to_res(&ico_bytes)
            .unwrap_or_else(|e| panic!("failed to compile {}: {e}", ico_path.display()));
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let res_path = out_dir.join("kiri_icon.res");
        fs::write(&res_path, res_bytes)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", res_path.display()));
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
    }
}
