// Кратко: родные значки рабочего стола Windows — спрятать на время работы программы, вернуть при выходе;
// узнать, где стоит значок папки (туда впервые ставится её плитка).
//
// Hide = ShowWindow(SW_HIDE) on the desktop list view. Nothing is moved, no setting is written:
// icon positions stay exactly as they were. Explorer restart also brings icons back (fresh view).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;

use windows::core::{w, Interface, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

thread_local! {
    /// Icons were visible before we hid them -> show again on exit. False: user had them off already.
    static HIDDEN: Cell<bool> = const { Cell::new(false) };
}

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

/// Hide native desktop icons (our tiles replace them).
/// Already hidden while the user setting says "show" = left over from a killed run -> ours to restore.
pub fn hide_icons() {
    if let Some(lv) = icon_view() {
        unsafe {
            if IsWindowVisible(lv).as_bool() || !user_hides_icons() {
                let _ = ShowWindow(lv, SW_HIDE);
                HIDDEN.with(|h| h.set(true));
            }
        }
    }
}

/// Bring native icons back (only if we hid them). Safe to call many times.
pub fn restore_icons() {
    if HIDDEN.with(|h| h.replace(false)) {
        if let Some(lv) = icon_view() {
            unsafe {
                let _ = ShowWindow(lv, SW_SHOW);
            }
        }
    }
}

/// Screen position (physical px, top-left of icon) of every desktop item, by full path.
/// Shell way (IShellWindows -> desktop IFolderView), no cross-process list view memory tricks.
pub fn icon_positions() -> HashMap<PathBuf, POINT> {
    let mut out = HashMap::new();
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
                if let Ok(p) = view.GetItemPosition(pidl) {
                    let mut sr = Common::STRRET::default();
                    if folder.GetDisplayNameOf(pidl, SHGDN_FORPARSING, &mut sr).is_ok() {
                        let mut s = PWSTR::null();
                        if StrRetToStrW(&mut sr, Some(pidl), &mut s).is_ok() {
                            let path = s.to_string().unwrap_or_default();
                            CoTaskMemFree(Some(s.0 as _));
                            out.insert(PathBuf::from(path), POINT { x: p.x + origin.x, y: p.y + origin.y });
                        }
                    }
                }
                CoTaskMemFree(Some(pidl as _));
            }
            Ok(())
        })();
    }
    out
}
