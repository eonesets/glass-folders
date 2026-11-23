//! Windows shell helpers for the shortcut right-click menu: recycle, rename, icon picker, uninstall, confirm.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, IPersistFile, CLSCTX_ALL, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS,
    RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::WindowsProgramming::{
    GetPrivateProfileStringW, WritePrivateProfileStringW,
};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IShellItem, IShellLinkW, PickIconDlg, SHChangeNotify,
    SHCreateItemFromParsingName, ShellExecuteW, ShellLink, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHCNE_UPDATEITEM, SHCNF_PATHW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, SW_SHOWNORMAL,
};

const MAX_PATH_LEN: usize = 260;

/// Move `path` to the Recycle Bin silently (no confirmation or progress UI).
/// Assumes COM (STA) is already initialized on the calling thread.
pub fn recycle(path: &Path) -> bool {
    let res: windows::core::Result<()> = unsafe {
        (|| {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            let flags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI;
            op.SetOperationFlags(flags)?;
            let p = HSTRING::from(path.as_os_str());
            let item: IShellItem = SHCreateItemFromParsingName(&p, None)?;
            op.DeleteItem(&item, None)?;
            op.PerformOperations()
        })()
    };
    if let Err(e) = res {
        eprintln!("glass-folders: recycle {}: {e}", path.display());
        return false;
    }
    !path.exists()
}

/// Rename `path` keeping its extension. Strips invalid Windows filename chars.
pub fn rename(path: &Path, new_stem: &str) -> Option<PathBuf> {
    let clean: String = new_stem
        .trim()
        .chars()
        .filter(|&c| {
            !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }

    let parent = path.parent()?;
    let mut new_name = OsString::from(clean);
    if let Some(ext) = path.extension() {
        if !ext.is_empty() {
            new_name.push(".");
            new_name.push(ext);
        }
    }

    if path.file_name() == Some(new_name.as_os_str()) {
        return Some(path.to_path_buf());
    }

    let target = parent.join(&new_name);
    let same_ci = path.file_name().is_some_and(|n| {
        n.to_string_lossy().to_lowercase() == new_name.to_string_lossy().to_lowercase()
    });
    if !same_ci && target.exists() {
        return None;
    }

    match std::fs::rename(path, &target) {
        Ok(()) => Some(target),
        Err(e) => {
            eprintln!("glass-folders: rename {} -> {}: {e}", path.display(), target.display());
            None
        }
    }
}

/// Show the system PickIconDlg for a `.lnk` or `.url` shortcut and persist the chosen icon.
pub fn pick_icon(owner: HWND, path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let changed = if ext.eq_ignore_ascii_case("lnk") {
        pick_icon_lnk(owner, path)
    } else if ext.eq_ignore_ascii_case("url") {
        pick_icon_url(owner, path)
    } else {
        return false;
    };
    if changed {
        unsafe {
            let p = HSTRING::from(path.as_os_str());
            SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW, Some(p.as_ptr() as _), None);
        }
    }
    changed
}

fn pick_icon_lnk(owner: HWND, path: &Path) -> bool {
    let res: windows::core::Result<bool> = unsafe {
        (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let pf: IPersistFile = link.cast()?;
            let hpath = HSTRING::from(path.as_os_str());
            pf.Load(&hpath, STGM_READ)?;

            let mut buf = [0u16; MAX_PATH_LEN];
            let mut idx = 0i32;
            let _ = link.GetIconLocation(&mut buf, &mut idx);
            if wide_to_string(&buf).is_empty() {
                buf.fill(0);
                idx = 0;
                let _ = link.GetPath(&mut buf, std::ptr::null_mut(), 0);
            }

            let hwnd = (!owner.is_invalid()).then_some(owner);
            if PickIconDlg(hwnd, &mut buf, Some(&mut idx)) == 0 {
                return Ok(false);
            }

            let chosen = HSTRING::from(wide_to_string(&buf));
            link.SetIconLocation(&chosen, idx)?;
            pf.Save(&hpath, true)?;
            Ok(true)
        })()
    };
    match res {
        Ok(ok) => ok,
        Err(e) => {
            eprintln!("glass-folders: pick_icon {}: {e}", path.display());
            false
        }
    }
}

fn pick_icon_url(owner: HWND, path: &Path) -> bool {
    unsafe {
        let hpath = HSTRING::from(path.as_os_str());
        let mut buf = [0u16; MAX_PATH_LEN];
        GetPrivateProfileStringW(
            w!("InternetShortcut"),
            w!("IconFile"),
            w!(""),
            Some(&mut buf),
            &hpath,
        );

        let mut idx_buf = [0u16; 16];
        GetPrivateProfileStringW(
            w!("InternetShortcut"),
            w!("IconIndex"),
            w!("0"),
            Some(&mut idx_buf),
            &hpath,
        );
        let mut idx = wide_to_string(&idx_buf).trim().parse::<i32>().unwrap_or(0);

        let hwnd = (!owner.is_invalid()).then_some(owner);
        if PickIconDlg(hwnd, &mut buf, Some(&mut idx)) == 0 {
            return false;
        }

        let chosen = HSTRING::from(wide_to_string(&buf));
        let idx_str = HSTRING::from(idx.to_string());
        if let Err(e) =
            WritePrivateProfileStringW(w!("InternetShortcut"), w!("IconFile"), &chosen, &hpath)
        {
            eprintln!("glass-folders: pick_icon {} (IconFile): {e}", path.display());
            return false;
        }
        if let Err(e) =
            WritePrivateProfileStringW(w!("InternetShortcut"), w!("IconIndex"), &idx_str, &hpath)
        {
            eprintln!("glass-folders: pick_icon {} (IconIndex): {e}", path.display());
            return false;
        }
        true
    }
}

/// Matched uninstall entry for an installed application.
pub struct Uninstall {
    pub name: String,
    pub cmd: String,
}

/// Find the installed program that a `.lnk` or `.exe` shortcut belongs to.
pub fn uninstaller(path: &Path) -> Option<Uninstall> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    let target = if ext.eq_ignore_ascii_case("lnk") {
        resolve_lnk(path)?
    } else if ext.eq_ignore_ascii_case("exe") {
        path.to_path_buf()
    } else {
        return None;
    };

    let target_norm = norm_path(&target.to_string_lossy());
    if target_norm.is_empty() {
        return None;
    }
    let fallback_name = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let views: [(HKEY, REG_SAM_FLAGS); 3] = [
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, KEY_READ),
    ];

    let mut best_loc: Option<(usize, Uninstall)> = None;

    for (root, sam) in views {
        let mut hparent = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall"),
                Some(0),
                sam,
                &mut hparent,
            )
        };
        if status != ERROR_SUCCESS {
            continue;
        }

        let mut index = 0u32;
        let mut name_buf = [0u16; 256];
        loop {
            let mut name_len = name_buf.len() as u32;
            let st = unsafe {
                RegEnumKeyExW(
                    hparent,
                    index,
                    Some(PWSTR(name_buf.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if st != ERROR_SUCCESS {
                break;
            }
            index += 1;

            let sub_name = HSTRING::from(String::from_utf16_lossy(&name_buf[..name_len as usize]));
            if reg_dword(hparent, &sub_name, w!("SystemComponent")) == Some(1) {
                continue;
            }
            let Some(cmd) = reg_string(hparent, &sub_name, w!("UninstallString")) else {
                continue;
            };
            let cmd = cmd.trim().to_string();
            if cmd.is_empty() {
                continue;
            }

            let display_name = reg_string(hparent, &sub_name, w!("DisplayName"))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback_name.clone());

            if let Some(icon) = reg_string(hparent, &sub_name, w!("DisplayIcon")) {
                if clean_display_icon(&icon) == target_norm {
                    unsafe {
                        let _ = RegCloseKey(hparent);
                    }
                    return Some(Uninstall { name: display_name, cmd });
                }
            }

            if let Some(loc) = reg_string(hparent, &sub_name, w!("InstallLocation")) {
                let loc_norm = norm_path(&loc);
                if !loc_norm.is_empty()
                    && !is_generic_dir(&loc_norm)
                    && path_is_inside(&target_norm, &loc_norm)
                {
                    let len = loc_norm.len();
                    if best_loc.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
                        best_loc = Some((len, Uninstall { name: display_name, cmd }));
                    }
                }
            }
        }

        unsafe {
            let _ = RegCloseKey(hparent);
        }
    }

    best_loc.map(|(_, u)| u)
}

/// Launch an uninstaller command via ShellExecuteW ("open").
pub fn run_uninstall(u: &Uninstall) -> bool {
    let (prog, args) = split_cmd(&u.cmd);
    if prog.is_empty() {
        return false;
    }
    unsafe {
        let hprog = HSTRING::from(prog);
        let hargs = HSTRING::from(args);
        let params = if args.is_empty() { PCWSTR::null() } else { PCWSTR(hargs.as_ptr()) };
        let r = ShellExecuteW(None, w!("open"), &hprog, params, PCWSTR::null(), SW_SHOWNORMAL);
        let code = r.0 as isize;
        if code <= 32 {
            eprintln!("glass-folders: run_uninstall {prog}: code {code}");
            return false;
        }
        true
    }
}

/// Open Windows Settings -> Installed apps (fallback when no uninstaller is found).
pub fn open_apps_settings() {
    unsafe {
        let r = ShellExecuteW(
            None,
            w!("open"),
            w!("ms-settings:appsfeatures"),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        let code = r.0 as isize;
        if code <= 32 {
            eprintln!("glass-folders: open_apps_settings: code {code}");
        }
    }
}

/// Warning Yes/No confirmation box with "No" as the default button.
pub fn confirm(owner: HWND, title: &str, text: &str) -> bool {
    unsafe {
        let hwnd = (!owner.is_invalid()).then_some(owner);
        let t = HSTRING::from(title);
        let m = HSTRING::from(text);
        MessageBoxW(hwnd, &m, &t, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
    }
}

// ---------- internal helpers ----------

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn resolve_lnk(path: &Path) -> Option<PathBuf> {
    let res: windows::core::Result<PathBuf> = unsafe {
        (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let pf: IPersistFile = link.cast()?;
            let hpath = HSTRING::from(path.as_os_str());
            pf.Load(&hpath, STGM_READ)?;
            let mut buf = [0u16; MAX_PATH_LEN];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0)?;
            Ok(PathBuf::from(wide_to_string(&buf)))
        })()
    };
    match res {
        Ok(p) if !p.as_os_str().is_empty() => Some(p),
        Ok(_) => None,
        Err(e) => {
            eprintln!("glass-folders: resolve_lnk {}: {e}", path.display());
            None
        }
    }
}

fn reg_dword(hkey: HKEY, subkey: &HSTRING, value: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut sz = 4u32;
    let st = unsafe {
        RegGetValueW(
            hkey,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut sz),
        )
    };
    (st == ERROR_SUCCESS).then_some(v)
}

fn reg_string(hkey: HKEY, subkey: &HSTRING, value: PCWSTR) -> Option<String> {
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut stack_buf = [0u16; 512];
    let mut sz = (stack_buf.len() * 2) as u32;
    let st = unsafe {
        RegGetValueW(
            hkey,
            subkey,
            value,
            flags,
            None,
            Some(stack_buf.as_mut_ptr() as _),
            Some(&mut sz),
        )
    };
    if st == ERROR_SUCCESS {
        return Some(wide_to_string(&stack_buf[..(sz as usize / 2).min(stack_buf.len())]));
    }
    if st != ERROR_MORE_DATA || sz == 0 {
        return None;
    }
    let mut heap_buf = vec![0u16; (sz as usize + 1) / 2];
    let st = unsafe {
        RegGetValueW(
            hkey,
            subkey,
            value,
            flags,
            None,
            Some(heap_buf.as_mut_ptr() as _),
            Some(&mut sz),
        )
    };
    if st == ERROR_SUCCESS {
        Some(wide_to_string(&heap_buf))
    } else {
        None
    }
}

/// Normalize a Windows path for case-insensitive comparison: lowercase, backslashes, no quotes or trailing slash.
fn norm_path(s: &str) -> String {
    let mut out = s.trim().trim_matches('"').trim().replace('/', "\\").to_lowercase();
    while out.ends_with('\\') {
        out.pop();
    }
    out
}

/// Strip quotes and trailing `,N` icon index from a registry DisplayIcon value, then normalize.
fn clean_display_icon(raw: &str) -> String {
    let mut s = raw.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s = &s[1..s.len() - 1];
    }
    if let Some((head, tail)) = s.rsplit_once(',') {
        if tail.trim().parse::<i32>().is_ok() {
            s = head;
        }
    }
    norm_path(s)
}

/// True if `dir` (normalized, no trailing backslash) is a drive root or a top-level system folder.
fn is_generic_dir(dir: &str) -> bool {
    let bytes = dir.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' {
        return true;
    }
    if bytes.len() > 3 && bytes[1] == b':' && bytes[2] == b'\\' {
        let rest = &dir[3..];
        if matches!(rest, "program files" | "program files (x86)" | "windows") {
            return true;
        }
    }
    false
}

fn path_is_inside(file: &str, dir: &str) -> bool {
    file.starts_with(dir) && file.as_bytes().get(dir.len()) == Some(&b'\\')
}

/// Split an uninstall command line into (program, args).
fn split_cmd(cmd: &str) -> (&str, &str) {
    let s = cmd.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (&rest[..end], rest[end + 1..].trim());
        }
        return (rest.trim(), "");
    }
    if let Some(pos) = s.to_ascii_lowercase().find(".exe") {
        let end = pos + 4;
        return (s[..end].trim(), s[end..].trim());
    }
    match s.split_once(char::is_whitespace) {
        Some((p, a)) => (p, a.trim()),
        None => (s, ""),
    }
}
