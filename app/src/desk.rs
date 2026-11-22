// Кратко: родные значки рабочего стола Windows. Папки, у которых есть плитка, на время работы
// программы получают атрибут «скрытый» (их значки пропадают со стола; ярлыки, выделение рамкой,
// меню — как обычно). При выходе атрибут снимается, значки встают на прежние места.
//
// v0.5: no more hiding the whole icon layer (lost selection + loose shortcuts).
// Tried: moving folder icons off-screen (SelectAndPositionItems / LVM_SETITEMPOSITION) -> Explorer
// clamps them into the visible area. So: FILE_ATTRIBUTE_HIDDEN (Explorer hides them, "show hidden
// files" is off by default). Home spots saved in settings -> a killed run is repaired on next start.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

const HIDDEN: u32 = 0x2; // FILE_ATTRIBUTE_HIDDEN

/// Desktop icon list view: Progman or a WorkerW -> SHELLDLL_DefView -> SysListView32.
fn icon_view() -> Option<HWND> {
    unsafe {
        let mut def = FindWindowW(w!("Progman"), None).ok().and_then(|p| FindWindowExW(Some(p), None, w!("SHELLDLL_DefView"), None).ok());
        if def.is_none() {
            // DefView moved into a WorkerW (wallpaper slideshow, Win+Tab once used, ...)
            unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
                unsafe {
                    if let Ok(d) = FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None) {
                        *(lp.0 as *mut Option<HWND>) = Some(d);
                        return false.into();
                    }
                }
                true.into()
            }
            let _ = EnumWindows(Some(each), LPARAM(&mut def as *mut _ as isize));
        }
        FindWindowExW(Some(def?), None, w!("SysListView32"), None).ok()
    }
}

/// User setting "Show desktop icons" (Explorer\Advanced\HideIcons = 1 means off).
fn user_hides_icons() -> bool {
    let mut v: u32 = 0;
    let mut sz = 4u32;
    unsafe {
        let _ = windows::Win32::System::Registry::RegGetValueW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"),
            w!("HideIcons"),
            windows::Win32::System::Registry::RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut sz),
        );
    }
    v != 0
}

/// v0.4 hid the whole icon layer. Left hidden by a killed v0.4 run (user setting says "show") -> show it.
pub fn unhide_icon_layer() {
    if let Some(lv) = icon_view() {
        unsafe {
            if !IsWindowVisible(lv).as_bool() && !user_hides_icons() {
                let _ = ShowWindow(lv, SW_SHOW);
            }
        }
    }
}

/// Run `f` for every item of the desktop shell view: (view, child pidl, full path, list view origin).
fn each_item(mut f: impl FnMut(&IFolderView, *mut ITEMIDLIST, PathBuf, POINT)) {
    unsafe {
        let _ = (|| -> windows::core::Result<()> {
            let sw: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
            let mut loc = VARIANT::default();
            (*loc.Anonymous.Anonymous).vt = VT_I4;
            (*loc.Anonymous.Anonymous).Anonymous.lVal = 0; // CSIDL_DESKTOP
            let empty = VARIANT::default();
            let mut hwnd = 0i32;
            let disp = sw.FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
            let sp: IServiceProvider = disp.cast()?;
            let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
            let view: IFolderView = browser.QueryActiveShellView()?.cast()?;
            let folder: IShellFolder = view.GetFolder()?;
            let items: IEnumIDList = view.Items(SVGIO_ALLVIEW)?;
            // item positions are list view client coords; list view origin = its screen corner
            let mut origin = POINT::default();
            if let Some(lv) = icon_view() {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(lv, &mut origin);
            }
            loop {
                let mut one: [*mut ITEMIDLIST; 1] = [std::ptr::null_mut()];
                let mut got = 0u32;
                if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                let pidl = one[0];
                let mut sr = Common::STRRET::default();
                if folder.GetDisplayNameOf(pidl, SHGDN_FORPARSING, &mut sr).is_ok() {
                    let mut s = PWSTR::null();
                    if StrRetToStrW(&mut sr, Some(pidl), &mut s).is_ok() {
                        let path = PathBuf::from(s.to_string().unwrap_or_default());
                        CoTaskMemFree(Some(s.0 as _));
                        f(&view, pidl, path, origin);
                    }
                }
                CoTaskMemFree(Some(pidl as _));
            }
            Ok(())
        })();
    }
}

/// Screen position (physical px, top-left of icon) of every desktop item, by full path.
pub fn icon_positions() -> HashMap<PathBuf, POINT> {
    let mut out = HashMap::new();
    each_item(|view, pidl, path, o| {
        if let Ok(p) = unsafe { view.GetItemPosition(pidl) } {
            out.insert(path, POINT { x: p.x + o.x, y: p.y + o.y });
        }
    });
    out
}

/// Move desktop icons to screen points. Returns how many were found and moved.
fn place(to: &HashMap<PathBuf, POINT>) -> usize {
    let mut n = 0;
    each_item(|view, pidl, path, o| {
        if let Some(p) = to.get(&path) {
            let pt = [POINT { x: p.x - o.x, y: p.y - o.y }];
            let ids = [pidl as *const ITEMIDLIST];
            if unsafe { view.SelectAndPositionItems(1, ids.as_ptr(), Some(pt.as_ptr()), SVSI_POSITIONITEM.0 as u32) }.is_ok() {
                n += 1;
            }
        }
    });
    n
}


thread_local! {
    /// Folders we hid -> home spots of their icons (screen px).
    static HOME: RefCell<HashMap<PathBuf, POINT>> = RefCell::new(HashMap::new());
}

/// Set / clear the hidden attribute; tell Explorer at once. Returns true if it changed.
fn set_hidden(dir: &Path, on: bool) -> bool {
    let Ok(m) = std::fs::metadata(dir) else { return false };
    let a = m.file_attributes();
    let n = if on { a | HIDDEN } else { a & !HIDDEN };
    if n == a {
        return false;
    }
    let path = HSTRING::from(dir.as_os_str());
    unsafe {
        if SetFileAttributesW(&path, FILE_FLAGS_AND_ATTRIBUTES(n)).is_err() {
            return false;
        }
        SHChangeNotify(SHCNE_ATTRIBUTES, SHCNF_PATHW, Some(path.as_ptr() as _), None);
    }
    true
}

/// Folder icons off the desktop (they have tiles now). `home` = where they stand now; kept for restore.
pub fn hide_folders(home: HashMap<PathBuf, POINT>) {
    for d in home.keys() {
        set_hidden(d, true);
    }
    HOME.with(|h| *h.borrow_mut() = home);
}

/// Folders visible again, icons back on their spots. Safe to call many times.
/// `extra`: spots saved by a killed run. Waits (max ~2 s) for Explorer to show the icons, then places them.
pub fn restore_folders(extra: &HashMap<PathBuf, POINT>) {
    let mut home = HOME.with(|h| std::mem::take(&mut *h.borrow_mut()));
    home.extend(extra.iter().map(|(k, v)| (k.clone(), *v)));
    home.retain(|d, _| d.exists());
    if home.is_empty() {
        return;
    }
    for d in home.keys() {
        set_hidden(d, false);
    }
    // Explorer adds them back asynchronously, at some free spot -> put each where it was
    for _ in 0..20 {
        if place(&home) == home.len() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
