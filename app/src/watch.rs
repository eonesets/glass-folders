// Кратко: следит за папкой рабочего стола в фоне. Появилась / пропала / переименована папка или файл —
// сообщает главному окну (оно само подождёт ~250 мс и обновит плитки). Свой файл .glassfolder.json не в счёт.
//
// ReadDirectoryChangesW, subtree, DIR_NAME | FILE_NAME only: NO attribute changes (the app hides tiled
// folders by attribute itself -> would loop). Blocking read on its own thread; dies with the process.

use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// Watch `desktop` (whole tree). `on_change(Some(entries))` = these desktop entries (top level) changed;
/// `on_change(None)` = too many changes to list, re-read everything. Called on the watcher thread.
pub fn start(desktop: PathBuf, on_change: impl Fn(Option<Vec<PathBuf>>) + Send + 'static) {
    let _ = std::thread::Builder::new().name("gf-watch".into()).spawn(move || watch(&desktop, on_change));
}

fn watch(desktop: &Path, on_change: impl Fn(Option<Vec<PathBuf>>)) {
    let h = unsafe {
        CreateFileW(
            &HSTRING::from(desktop.as_os_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let Ok(h) = h else {
        eprintln!("glass-folders: cannot watch {}", desktop.display());
        return;
    };
    let mut buf = vec![0u32; 16 * 1024]; // 64 KB, DWORD aligned as the API wants
    loop {
        let mut got = 0u32;
        let r = unsafe {
            ReadDirectoryChangesW(
                h,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 4) as u32,
                true,
                FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_FILE_NAME,
                Some(&mut got),
                None,
                None,
            )
        };
        if r.is_err() {
            break; // desktop folder itself gone / handle broken
        }
        if got == 0 {
            on_change(None); // buffer overflow: changes lost, re-read all
            continue;
        }
        let rels = parse(&buf, got as usize);
        let tops: Vec<PathBuf> = rels
            .iter()
            .filter(|r| !crate::model::ignored_change(r))
            .filter_map(|r| r.components().next().map(|c| desktop.join(c)))
            .collect();
        if tops.is_empty() {
            continue; // only our own config files
        }
        on_change(Some(tops));
    }
    unsafe {
        let _ = CloseHandle(h);
    }
}

/// FILE_NOTIFY_INFORMATION records -> relative paths (NextEntryOffset, Action, FileNameLength, FileName).
fn parse(buf: &[u32], got: usize) -> Vec<PathBuf> {
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got.min(buf.len() * 4)) };
    let rd = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as usize;
    let mut out = Vec::new();
    let mut off = 0;
    while off + 12 <= bytes.len() {
        let (next, len) = (rd(off), rd(off + 8));
        let start = off + 12;
        if start + len > bytes.len() {
            break;
        }
        let name: Vec<u16> = bytes[start..start + len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push(PathBuf::from(std::ffi::OsString::from_wide(&name)));
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}
