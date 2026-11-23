// Кратко: «сторож». Пока программа прячет папки рабочего стола, рядом живёт маленький второй процесс.
// Если программу убили (Диспетчер задач, вылет, taskkill), сторож сразу возвращает папки на места.
//
// Why: TerminateProcess runs no code in the killed process -> hidden folders stayed hidden until the next start.
// Chain: app -> `--guard-start PID` (launcher, exits at once) -> `--guard PID`. The launcher exits, so the guard
// is not in the app's process tree ("End process tree" / taskkill /T leave it alive).
// Guard: waits for PID to end. Clean exit cleared settings.homes -> nothing to do. Else, unless a newer app
// instance holds the main mutex (it repairs on its own start), restores homes from settings and clears them.
// Not covered: killing both processes at once (taskkill /IM glass-folders.exe /F) -> next start repairs.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Foundation::POINT;
use windows::Win32::System::Threading::{CreateMutexW, OpenMutexW, OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE};

use crate::{desk, model, win};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// App instance marker: held for the whole run (never closed), gone when the process ends.
pub fn mark_running() {
    unsafe {
        // handle deliberately never closed (HANDLE is Copy: dropping it closes nothing)
        let _ = CreateMutexW(None, false, w!("Local\\GlassFolders.Main"));
    }
}

fn app_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\GlassFolders.Main")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

fn spawn(args: &[String]) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let flags = DETACHED_PROCESS | CREATE_NO_WINDOW;
    // out of our job object when allowed (a job may kill its members together)
    Command::new(&exe).args(args).creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok()
        || Command::new(&exe).args(args).creation_flags(flags).spawn().is_ok()
}

/// App side: start the guard for this process (call once the folders are hidden).
pub fn start() {
    if !spawn(&["--guard-start".into(), std::process::id().to_string()]) {
        eprintln!("glass-folders: guard not started");
    }
}

/// `--guard-start PID` / `--guard PID`: handled before any UI. Returns true if this run was one of them.
pub fn handle_args(args: &[String]) -> bool {
    let pid = args.get(2).and_then(|p| p.parse::<u32>().ok());
    match (args.get(1).map(String::as_str), pid) {
        (Some("--guard-start"), Some(pid)) => {
            spawn(&["--guard".into(), pid.to_string()]);
            true
        }
        (Some("--guard"), Some(pid)) => {
            watch(pid);
            true
        }
        _ => false,
    }
}

fn watch(pid: u32) {
    unsafe {
        let Ok(h): Result<HANDLE, _> = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return };
        WaitForSingleObject(h, INFINITE);
        let _ = CloseHandle(h);
    }
    // app gone. Clean exit -> homes empty. A newer instance running -> it restored them itself.
    let mut s = model::load_settings();
    if s.homes.is_empty() || app_running() {
        return;
    }
    win::com_init();
    let homes: HashMap<PathBuf, POINT> = s.homes.iter().map(|(k, [x, y])| (PathBuf::from(k), POINT { x: *x, y: *y })).collect();
    desk::restore_folders(&homes);
    if !app_running() {
        s.homes.clear();
        model::save_settings(&s);
    }
}
